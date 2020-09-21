// Copyright (c) 2020 kprotty
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use core::{
    cell::{Cell, UnsafeCell},
    fmt,
    future::Future,
    hint::unreachable_unchecked,
    marker::{PhantomData, PhantomPinned},
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
    pin::Pin,
    ptr::{drop_in_place, read, write, NonNull},
    sync::atomic::{fence, spin_loop_hint, AtomicU8, AtomicUsize, Ordering},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

use super::ThreadParker;

pub struct Lock<T> {
    state: AtomicUsize,
    value: UnsafeCell<T>,
}

impl<T: fmt::Debug> fmt::Debug for Lock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut f = f.debug_struct("Lock");
        let f = match self.try_lock() {
            Some(guard) => f.field("value", &&*guard),
            None => f.field("state", &"<locked>"),
        };
        f.finish()
    }
}

impl<T: Default> Default for Lock<T> {
    fn default() -> Self {
        Self::from(T::default())
    }
}

impl<T> From<T> for Lock<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T> AsMut<T> for Lock<T> {
    fn as_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }
}

unsafe impl<T: Send> Send for Lock<T> {}
unsafe impl<T: Send> Sync for Lock<T> {}

const UNLOCKED: u8 = 0;
const LOCKED: u8 = 1 << 0;
const WAKING: usize = 1 << 8;
const WAITING: usize = !(LOCKED as usize | WAKING);

impl<T> Lock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED as usize),
            value: UnsafeCell::new(value),
        }
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    #[inline]
    fn racy_wake() -> bool {
        super::cpu::is_intel()
    }

    #[inline]
    fn byte_state(&self) -> &AtomicU8 {
        unsafe { &*(&self.state as *const _ as *const _) }
    }

    #[inline]
    pub fn try_lock(&self) -> Option<LockGuard<'_, T>> {
        match self.byte_state().swap(LOCKED, Ordering::Acquire) {
            UNLOCKED => Some(LockGuard(self)),
            _ => None,
        }
    }

    #[inline]
    pub fn lock_fast(&self) -> Result<LockGuard<'_, T>, LockFuture<'_, T>> {
        self.try_lock().ok_or_else(|| LockFuture::new(self))
    }

    pub fn lock_async(&self) -> LockAsyncFuture<'_, T> {
        LockAsyncFuture(Some(AsyncState::TryLock(self)))
    }

    #[inline]
    pub fn lock<P: ThreadParker>(&self) -> LockGuard<'_, T> {
        match self.lock_fast() {
            Ok(guard) => guard,
            Err(future) => unsafe { Self::lock_slow::<P>(future) },
        }
    }

    #[cold]
    unsafe fn lock_slow<'a, P: ThreadParker>(future: LockFuture<'a, T>) -> LockGuard<'a, T> {
        struct ParkWaker<P: ThreadParker>(PhantomData<*mut P>);
        impl<P: ThreadParker> ParkWaker<P> {
            const VTABLE: RawWakerVTable =
                RawWakerVTable::new(Self::clone, Self::wake, Self::wake, |_| {});

            unsafe fn clone(ptr: *const ()) -> RawWaker {
                (&*(ptr as *const P)).prepare_park();
                RawWaker::new(ptr, &Self::VTABLE)
            }

            unsafe fn wake(ptr: *const ()) {
                (&*(ptr as *const P)).unpark()
            }
        }

        let mut future = future;
        let parker = P::new();
        let parker_ptr = &parker as *const _ as *const ();
        let waker = Waker::from_raw(RawWaker::new(parker_ptr, &ParkWaker::<P>::VTABLE));

        loop {
            let mut context = Context::from_waker(&waker);
            match Pin::new_unchecked(&mut future).poll(&mut context) {
                Poll::Pending => parker.park(),
                Poll::Ready(guard) => return guard,
            }
        }
    }

    #[inline]
    pub unsafe fn unlock(&self) {
        self.byte_state().store(UNLOCKED, Ordering::Release);

        let state = self.state.load(Ordering::Relaxed);
        if state != (UNLOCKED as usize) {
            self.unlock_slow();
        }
    }

    #[cold]
    unsafe fn unlock_slow(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if (state & WAITING == 0) || (state & (LOCKED as usize | WAKING) != 0) {
                return;
            }
            match self.state.compare_exchange_weak(
                state,
                state | WAKING,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(e) => state = e,
            }
        }

        state |= WAKING;
        let racy_wake = Self::racy_wake();

        loop {
            let head = &*((state & WAITING) as *const Waiter);
            let tail = head.find_tail();

            if state & (LOCKED as usize) != 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state & !WAKING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return,
                    Err(e) => state = e,
                }
                continue;
            }

            if let Some(new_tail_ptr) = tail.prev.get().assume_init() {
                head.tail.set(MaybeUninit::new(Some(new_tail_ptr)));
                if racy_wake {
                    self.state.fetch_and(!WAKING, Ordering::Release);
                } else {
                    fence(Ordering::Release);
                }
            } else if let Err(e) = self.state.compare_exchange_weak(
                state,
                if racy_wake { UNLOCKED as usize } else { WAKING },
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                state = e;
                continue;
            }

            let wake_state = if racy_wake { WAKER_EMPTY } else { WAKER_WAKING };
            state = tail.state.load(Ordering::Relaxed);
            loop {
                if state == WAKER_UPDATING {
                    match tail.state.compare_exchange_weak(
                        WAKER_UPDATING,
                        wake_state,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => return,
                        Err(e) => state = e,
                    }
                    continue;
                }

                assert_eq!(state, WAKER_WAITING, "invalid waiter state");
                if let Err(e) = tail.state.compare_exchange_weak(
                    WAKER_WAITING,
                    WAKER_CONSUMING,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    state = e;
                    continue;
                }

                let waker = read(tail.waker_ptr());
                tail.state.store(wake_state, Ordering::Release);
                waker.wake();
                return;
            }
        }
    }
}

const WAKER_EMPTY: usize = 0;
const WAKER_WAITING: usize = 1;
const WAKER_UPDATING: usize = 2;
const WAKER_CONSUMING: usize = 3;
const WAKER_WAKING: usize = 4;

#[repr(align(512))]
struct Waiter {
    prev: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    next: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    tail: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    waker: UnsafeCell<MaybeUninit<Waker>>,
    state: AtomicUsize,
}

impl Waiter {
    fn new() -> Self {
        Self {
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
            waker: UnsafeCell::new(MaybeUninit::uninit()),
            state: AtomicUsize::new(WAKER_EMPTY),
        }
    }

    unsafe fn waker_ptr(&self) -> *mut Waker {
        (*self.waker.get()).as_ptr() as *mut Waker
    }

    unsafe fn find_tail(&self) -> &Self {
        let tail_ptr = self.tail.get().assume_init().unwrap_or_else(|| {
            let mut current = NonNull::from(self);
            loop {
                let next = current.as_ref().next.get().assume_init();
                let next = next.unwrap_or_else(|| unreachable_unchecked());
                next.as_ref().prev.set(MaybeUninit::new(Some(current)));
                current = next;
                if let Some(tail) = current.as_ref().tail.get().assume_init() {
                    self.tail.set(MaybeUninit::new(Some(tail)));
                    break tail;
                }
            }
        });
        &*tail_ptr.as_ptr()
    }
}

enum AsyncState<'a, T> {
    TryLock(&'a Lock<T>),
    Poll(LockFuture<'a, T>),
}

pub struct LockAsyncFuture<'a, T>(Option<AsyncState<'a, T>>);

impl<'a, T> Drop for LockAsyncFuture<'a, T> {
    fn drop(&mut self) {
        if matches!(self.0, Some(AsyncState::Poll(_))) {
            unreachable!("LockAsyncFuture does not support cancellation");
        }
    }
}

impl<'a, T> Future for LockAsyncFuture<'a, T> {
    type Output = LockGuard<'a, T>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe {
            let mut_self = Pin::into_inner_unchecked(self);
            loop {
                let guard = match mut_self.0 {
                    None => unreachable!("LockAsyncFuture polled after completion"),
                    Some(AsyncState::TryLock(lock)) => match lock.lock_fast() {
                        Ok(guard) => guard,
                        Err(future) => {
                            mut_self.0 = Some(AsyncState::Poll(future));
                            continue;
                        }
                    },
                    Some(AsyncState::Poll(ref mut future)) => {
                        match Pin::new_unchecked(future).poll(ctx) {
                            Poll::Pending => return Poll::Pending,
                            Poll::Ready(guard) => guard,
                        }
                    }
                };

                mut_self.0 = None;
                return Poll::Ready(guard);
            }
        }
    }
}

pub struct LockFuture<'a, T> {
    lock: Option<&'a Lock<T>>,
    waiter: UnsafeCell<Waiter>,
    _pinned: PhantomPinned,
}

impl<'a, T> LockFuture<'a, T> {
    fn new(lock: &'a Lock<T>) -> Self {
        Self {
            lock: Some(lock),
            waiter: UnsafeCell::new(Waiter::new()),
            _pinned: PhantomPinned,
        }
    }
}

impl<'a, T> Drop for LockFuture<'a, T> {
    fn drop(&mut self) {
        if self.lock.is_some() {
            unreachable!("LockFuture does not support cancellation");
        }
    }
}

impl<'a, T> Future for LockFuture<'a, T> {
    type Output = LockGuard<'a, T>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe {
            let mut_self = Pin::into_inner_unchecked(self);
            let lock = mut_self.lock.expect("LockFuture polled after completion");
            let waiter = &*mut_self.waiter.get();
            let waker_ptr = waiter.waker_ptr();

            let is_waking = match waiter.state.load(Ordering::Relaxed) {
                WAKER_EMPTY => false,
                WAKER_WAKING => true,
                WAKER_CONSUMING => return Poll::Pending,
                WAKER_WAITING => match waiter.state.compare_exchange(
                    WAKER_WAITING,
                    WAKER_UPDATING,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Err(waker_state) => match waker_state {
                        WAKER_EMPTY => false,
                        WAKER_WAKING => true,
                        WAKER_CONSUMING => return Poll::Pending,
                        _ => unreachable!("invalid updated waker state"),
                    },
                    Ok(_) => {
                        drop_in_place(waker_ptr);
                        write(waker_ptr, ctx.waker().clone());

                        match waiter.state.compare_exchange(
                            WAKER_UPDATING,
                            WAKER_WAITING,
                            Ordering::Release,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => return Poll::Pending,
                            Err(waker_state) => {
                                drop_in_place(waker_ptr);
                                match waker_state {
                                    WAKER_EMPTY => false,
                                    WAKER_WAKING => true,
                                    _ => unreachable!("invalid updated waker state"),
                                }
                            }
                        }
                    }
                },
                _ => unreachable!("invalid waker state"),
            };

            let mut has_waker = false;
            let mut spin = super::Spin::new();
            let mut state = lock.state.load(Ordering::Relaxed);

            let guard = loop {
                let is_unlocked = state & (LOCKED as usize) == 0;
                if !is_waking && is_unlocked {
                    match lock.try_lock() {
                        Some(guard) => break guard,
                        _ => {
                            spin_loop_hint();
                            state = lock.state.load(Ordering::Relaxed);
                            continue;
                        }
                    }
                }

                let mut new_state = state;
                let head = NonNull::new((state & WAITING) as *mut Waiter);

                if is_unlocked {
                    new_state |= LOCKED as usize;
                } else if head.is_none() && spin.yield_now() {
                    state = lock.state.load(Ordering::Relaxed);
                    continue;
                } else {
                    new_state = (new_state & !WAITING) | (waiter as *const _ as usize);
                    waiter.next.set(MaybeUninit::new(head));
                    waiter.tail.set(MaybeUninit::new(match head {
                        Some(_) => None,
                        None => Some(NonNull::from(waiter)),
                    }));
                    has_waker = has_waker || {
                        write(waker_ptr, ctx.waker().clone());
                        waiter.prev.set(MaybeUninit::new(None));
                        waiter.state.store(WAKER_WAITING, Ordering::Relaxed);
                        true
                    };
                }

                if is_waking {
                    new_state &= !WAKING;
                }

                if let Err(e) = lock.state.compare_exchange_weak(
                    state,
                    new_state,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    state = e;
                    continue;
                }

                if is_unlocked {
                    break LockGuard(lock);
                } else {
                    return Poll::Pending;
                }
            };

            if has_waker {
                drop_in_place(waker_ptr);
            }

            mut_self.lock = None;
            return Poll::Ready(guard);
        }
    }
}

pub struct LockGuard<'a, T>(&'a Lock<T>);

impl<'a, T> Drop for LockGuard<'a, T> {
    fn drop(&mut self) {
        unsafe { self.0.unlock() }
    }
}

impl<'a, T: fmt::Debug> fmt::Debug for LockGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(f)
    }
}

impl<'a, T> Deref for LockGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.0.value.get() }
    }
}

impl<'a, T> DerefMut for LockGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.0.value.get() }
    }
}
