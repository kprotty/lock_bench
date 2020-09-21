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
    cell::UnsafeCell,
    convert::TryInto,
    fmt,
    future::Future,
    marker::PhantomPinned,
    ops::{Deref, DerefMut},
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicU16, AtomicU8, Ordering},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
    time::Duration,
};

use crate::core::{
    wait_queue::{List, Node},
    Lock, ThreadParker,
};

#[derive(Default)]
struct Prng(u16);

impl Prng {
    fn xorshift16(&mut self) -> u16 {
        self.0 ^= self.0 << 7;
        self.0 ^= self.0 >> 9;
        self.0 ^= self.0 << 8;
        self.0
    }

    fn gen_u32(&mut self) -> u32 {
        let a = self.xorshift16() as u32;
        let b = self.xorshift16() as u32;
        (a << 16) | b
    }
}

enum Notified {
    Retry,
    Acquired,
}

struct Waiting<P: ThreadParker> {
    waker: Waker,
    force_fair_at: P::Instant,
}

enum WaitState<P: ThreadParker> {
    Empty,
    Waiting(Waiting<P>),
    Notified(Notified),
}

pub struct Mutex<P: ThreadParker, T> {
    state: AtomicU8,
    prng: AtomicU16,
    queue: Lock<List<WaitState<P>>>,
    value: UnsafeCell<T>,
}

impl<P: ThreadParker, T: fmt::Debug> fmt::Debug for Mutex<P, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut f = f.debug_struct("Mutex");
        let f = match self.try_lock() {
            Some(guard) => f.field("value", &&*guard),
            None => f.field("state", &"<locked>"),
        };
        f.finish()
    }
}

impl<P: ThreadParker, T: Default> Default for Mutex<P, T> {
    fn default() -> Self {
        Self::from(T::default())
    }
}

impl<P: ThreadParker, T> From<T> for Mutex<P, T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<P: ThreadParker, T> AsMut<T> for Mutex<P, T> {
    fn as_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }
}

unsafe impl<P: ThreadParker, T: Send> Send for Mutex<P, T> {}
unsafe impl<P: ThreadParker, T: Send> Sync for Mutex<P, T> {}

const UNLOCKED: u8 = 0;
const LOCKED: u8 = 1 << 0;
const PARKED: u8 = 1 << 1;

impl<P: ThreadParker, T> Mutex<P, T> {
    pub fn new(value: T) -> Self {
        Self {
            state: AtomicU8::new(UNLOCKED),
            prng: AtomicU16::new(Prng::default().0),
            queue: Lock::new(List::new()),
            value: UnsafeCell::new(value),
        }
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    fn with_queue<F>(&self, f: impl FnOnce(&mut List<WaitState<P>>) -> F) -> F {
        f(&mut *self.queue.lock::<P>())
    }

    #[inline]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, P, T>> {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & LOCKED != 0 {
                return None;
            }
            match self.state.compare_exchange_weak(
                state,
                state | LOCKED,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(MutexGuard(self)),
                Err(e) => state = e,
            }
        }
    }

    #[inline]
    fn try_lock_fast(&self) -> Option<MutexGuard<'_, P, T>> {
        self.state
            .compare_exchange_weak(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| MutexGuard(self))
    }

    #[inline]
    pub fn try_lock_for(&self, timeout: Duration) -> Option<MutexGuard<'_, P, T>> {
        self.try_lock_until(P::now() + timeout)
    }

    #[inline]
    pub fn try_lock_until(&self, deadline: P::Instant) -> Option<MutexGuard<'_, P, T>> {
        self.lock_sync(|parker| {
            parker.park_until(deadline);
            P::now() < deadline
        })
    }

    #[inline]
    pub fn lock(&self) -> MutexGuard<'_, P, T> {
        self.lock_sync(|parker| {
            parker.park();
            true
        })
        .unwrap()
    }

    #[inline]
    fn lock_sync(&self, try_park: impl Fn(&P) -> bool) -> Option<MutexGuard<'_, P, T>> {
        self.try_lock_fast()
            .or_else(|| self.lock_sync_slow(try_park))
    }

    const VTABLE: RawWakerVTable = RawWakerVTable::new(
        Self::vtable_clone,
        Self::vtable_wake,
        Self::vtable_wake,
        |_| {},
    );

    unsafe fn vtable_clone(ptr: *const ()) -> RawWaker {
        (&*(ptr as *const P)).prepare_park();
        RawWaker::new(ptr, &Self::VTABLE)
    }

    unsafe fn vtable_wake(ptr: *const ()) {
        (&*(ptr as *const P)).unpark()
    }

    #[cold]
    fn lock_sync_slow(&self, try_park: impl Fn(&P) -> bool) -> Option<MutexGuard<'_, P, T>> {
        let parker = P::new();
        let waker = unsafe {
            let parker_ptr = &parker as *const _ as *const ();
            let raw_waker = RawWaker::new(parker_ptr, &Self::VTABLE);
            Waker::from_raw(raw_waker)
        };

        let mut context = Context::from_waker(&waker);
        let mut future = MutexLockFuture {
            waiter: UnsafeCell::new(Node::new(WaitState::Empty)),
            mutex: MutexState::Locking(self),
            _pinned: PhantomPinned,
        };

        loop {
            let pinned_future = unsafe { Pin::new_unchecked(&mut future) };
            match pinned_future.poll(&mut context) {
                Poll::Pending if !try_park(&parker) => return None,
                Poll::Ready(guard) => return Some(guard),
                _ => continue,
            }
        }
    }

    pub fn lock_async(&self) -> MutexLockFuture<'_, P, T> {
        MutexLockFuture {
            waiter: UnsafeCell::new(Node::new(WaitState::Empty)),
            mutex: MutexState::TryLock(self),
            _pinned: PhantomPinned,
        }
    }

    #[inline]
    pub unsafe fn unlock(&self) {
        self.unlock_fast(false);
    }

    #[inline]
    pub unsafe fn unlock_fair(&self) {
        self.unlock_fast(true)
    }

    #[inline]
    fn unlock_fast(&self, force_fair: bool) {
        if self
            .state
            .compare_exchange(LOCKED, UNLOCKED, Ordering::Release, Ordering::Relaxed)
            .is_err()
        {
            self.unlock_slow(force_fair);
        }
    }

    #[cold]
    fn unlock_slow(&self, force_fair: bool) {
        if let Some(waker) = self.with_queue(|queue| unsafe {
            queue
                .pop_front()
                .map(|waiter| {
                    let wait_state = &mut *waiter.as_ref().get();
                    let Waiting {
                        force_fair_at,
                        waker,
                    } = match std::mem::replace(wait_state, WaitState::Notified(Notified::Retry)) {
                        WaitState::Waiting(waiting) => waiting,
                        _ => unreachable!("invalid wait state"),
                    };

                    let has_more = !queue.is_empty();
                    if force_fair || (P::now() >= force_fair_at) {
                        *wait_state = WaitState::Notified(Notified::Acquired);
                        if !has_more {
                            self.state.store(LOCKED, Ordering::Relaxed);
                        }
                    } else {
                        let new_state = if has_more { PARKED } else { UNLOCKED };
                        self.state.store(new_state, Ordering::Release);
                    }

                    waker
                })
                .or_else(|| {
                    self.state.store(UNLOCKED, Ordering::Release);
                    None
                })
        }) {
            waker.wake()
        }
    }
}

enum MutexState<'a, P: ThreadParker, T> {
    TryLock(&'a Mutex<P, T>),
    Locking(&'a Mutex<P, T>),
    Waiting(&'a Mutex<P, T>),
    Locked,
}

pub struct MutexLockFuture<'a, P: ThreadParker, T> {
    waiter: UnsafeCell<Node<WaitState<P>>>,
    mutex: MutexState<'a, P, T>,
    _pinned: PhantomPinned,
}

impl<'a, P: ThreadParker, T> Future for MutexLockFuture<'a, P, T> {
    type Output = MutexGuard<'a, P, T>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut_self = unsafe { Pin::into_inner_unchecked(self) };

        let guard = loop {
            match mut_self.mutex {
                MutexState::TryLock(mutex) => match mutex.try_lock_fast() {
                    Some(guard) => break guard,
                    None => {
                        mut_self.mutex = MutexState::Locking(mutex);
                        continue;
                    }
                },
                MutexState::Locking(mutex) => match mut_self.poll_lock(ctx, mutex) {
                    Some(guard) => break guard,
                    None => {
                        mut_self.mutex = MutexState::Waiting(mutex);
                        return Poll::Pending;
                    }
                },
                MutexState::Waiting(mutex) => match mut_self.poll_wait(ctx, mutex) {
                    None => return Poll::Pending,
                    Some(Notified::Acquired) => break MutexGuard(mutex),
                    Some(Notified::Retry) => mut_self.mutex = MutexState::Locking(mutex),
                },
                MutexState::Locked => {
                    unreachable!("MutexLockFuture polled after completion");
                }
            }
        };

        mut_self.mutex = MutexState::Locked;
        Poll::Ready(guard)
    }
}

impl<'a, P: ThreadParker, T> Drop for MutexLockFuture<'a, P, T> {
    fn drop(&mut self) {
        match self.mutex {
            MutexState::Waiting(mutex) => self.poll_cancel(mutex),
            _ => {}
        }
    }
}

impl<'a, P: ThreadParker, T> MutexLockFuture<'a, P, T> {
    fn poll_lock(
        &self,
        ctx: &mut Context<'_>,
        mutex: &'a Mutex<P, T>,
    ) -> Option<MutexGuard<'a, P, T>> {
        let mut spin = crate::core::Spin::new();
        let mut state = mutex.state.load(Ordering::Relaxed);
        loop {
            if state & LOCKED == 0 {
                match mutex.state.compare_exchange_weak(
                    state,
                    state | LOCKED,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return Some(MutexGuard(mutex)),
                    Err(e) => state = e,
                }
                continue;
            }

            if state & PARKED == 0 {
                if spin.yield_now() {
                    state = mutex.state.load(Ordering::Relaxed);
                    continue;
                } else if let Err(e) = mutex.state.compare_exchange_weak(
                    state,
                    state | PARKED,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    state = e;
                    continue;
                }
            }

            match mutex.with_queue(|queue| unsafe {
                if mutex.state.load(Ordering::Relaxed) != (LOCKED | PARKED) {
                    return false;
                }

                let waiter = NonNull::new_unchecked(self.waiter.get());
                let wait_state = &mut *waiter.as_ref().get();
                *wait_state = match wait_state {
                    WaitState::Empty => WaitState::Waiting(Waiting {
                        waker: ctx.waker().clone(),
                        force_fair_at: P::now()
                            + Duration::new(0, {
                                let mut prng = Prng(match mutex.prng.load(Ordering::Relaxed) {
                                    0 => (((wait_state as *mut _ as usize) >> 16) & 0xffff).try_into().unwrap(),
                                    prng_state => prng_state,
                                });
                                let rng_state = prng.gen_u32();
                                mutex.prng.store(prng.0, Ordering::Relaxed);
                                (rng_state % 1_000_000).try_into().unwrap()
                            }),
                    }),
                    _ => unreachable!("invalid wait state"),
                };

                queue.push_back(NonNull::from(waiter));
                true
            }) {
                false => state = mutex.state.load(Ordering::Relaxed),
                true => return None,
            }
        }
    }

    fn poll_wait(&self, ctx: &mut Context<'_>, mutex: &'a Mutex<P, T>) -> Option<Notified> {
        mutex.with_queue(|_queue| unsafe {
            let waiter = NonNull::new_unchecked(self.waiter.get());
            let wait_state = &mut *waiter.as_ref().get();

            if let WaitState::Waiting(ref mut waiting) = wait_state {
                waiting.waker = ctx.waker().clone();
                None
            } else if let WaitState::Notified(notified) =
                std::mem::replace(wait_state, WaitState::Empty)
            {
                Some(notified)
            } else {
                unreachable!("invalid wait state")
            }
        })
    }

    fn poll_cancel(&self, mutex: &'a Mutex<P, T>) {
        mutex.with_queue(|queue| unsafe {
            let waiter = NonNull::new_unchecked(self.waiter.get());
            let wait_state = &mut *waiter.as_ref().get();

            match std::mem::replace(wait_state, WaitState::Empty) {
                WaitState::Notified(_) => {}
                WaitState::Empty => unreachable!("invalid wait state"),
                WaitState::Waiting(_) => {
                    assert!(queue.try_remove(waiter));
                    if queue.is_empty() {
                        mutex.state.fetch_and(!PARKED, Ordering::Relaxed);
                    }
                }
            }
        });
    }
}

pub struct MutexGuard<'a, P: ThreadParker, T>(&'a Mutex<P, T>);

impl<'a, P: ThreadParker, T> MutexGuard<'a, P, T> {
    pub fn unlock_fair(self: Self) {
        unsafe { self.0.unlock_fair() }
    }
}

impl<'a, P: ThreadParker, T> Drop for MutexGuard<'a, P, T> {
    fn drop(&mut self) {
        unsafe { self.0.unlock() }
    }
}

impl<'a, P: ThreadParker, T: fmt::Debug> fmt::Debug for MutexGuard<'a, P, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(f)
    }
}

impl<'a, P: ThreadParker, T> Deref for MutexGuard<'a, P, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.0.value.get() }
    }
}

impl<'a, P: ThreadParker, T> DerefMut for MutexGuard<'a, P, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.0.value.get() }
    }
}
