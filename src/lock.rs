use super::{SpinWait, ThreadParker as Parker};
use core::{
    cell::{Cell, UnsafeCell},
    fmt,
    future::Future,
    marker::{PhantomData, PhantomPinned},
    ops::{Deref, DerefMut},
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

pub struct Lock<T> {
    state: AtomicUsize,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for Lock<T> {}
unsafe impl<T: Send> Sync for Lock<T> {}

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

impl<T: fmt::Debug> fmt::Debug for Lock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut f = f.debug_struct("Lock");
        match self.try_lock() {
            Some(guard) => f.field("value", &&*guard),
            None => f.field("state", &"<locked>"),
        }
        .finish()
    }
}

const UNLOCKED: usize = 0;
const LOCKED: usize = 1;
const WAKING: usize = 2;
const WAITING: usize = !(LOCKED | WAKING);

impl<T> Lock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED),
            value: UnsafeCell::new(value),
        }
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    #[inline]
    pub fn is_locked(&self) -> bool {
        let state = self.state.load(Ordering::Relaxed);
        state & LOCKED != 0
    }

    #[inline]
    pub fn try_lock(&self) -> Option<LockGuard<'_, T>> {
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
                Ok(_) => return Some(LockGuard(self)),
                Err(e) => state = e,
            }
        }
    }

    #[inline]
    pub fn lock<P: Parker>(&self) -> LockGuard<'_, T> {
        self.lock_fast().unwrap_or_else(|| self.lock_slow::<P>())
    }

    #[inline]
    fn lock_fast(&self) -> Option<LockGuard<'_, T>> {
        self.state
            .compare_exchange_weak(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| LockGuard(self))
    }

    #[cold]
    fn lock_slow<P: Parker>(&self) -> LockGuard<'_, T> {
        struct ParkWaker<P: Parker>(PhantomData<*mut P>);

        impl<P: Parker> ParkWaker<P> {
            const VTABLE: RawWakerVTable = RawWakerVTable::new(
                |ptr| unsafe {
                    (&*(ptr as *const P)).prepare_park();
                    RawWaker::new(ptr, &Self::VTABLE)
                },
                |ptr| unsafe { (&*(ptr as *const P)).unpark() },
                |ptr| unsafe { (&*(ptr as *const P)).unpark() },
                |_ptr| {},
            );
        }

        let parker = P::new();
        let waker = unsafe {
            let ptr = &parker as *const _ as *const ();
            let raw_waker = RawWaker::new(ptr, &ParkWaker::<P>::VTABLE);
            Waker::from_raw(raw_waker)
        };

        let mut context = Context::from_waker(&waker);
        let mut future = LockFuture::new(LockState::LockSlow(self));

        loop {
            let pinned_future = unsafe { Pin::new_unchecked(&mut future) };
            match pinned_future.poll(&mut context) {
                Poll::Ready(guard) => return guard,
                Poll::Pending => parker.park(),
            }
        }
    }

    #[inline]
    pub fn lock_async(&self) -> LockFuture<'_, T> {
        LockFuture::new(LockState::LockFast(self))
    }

    #[inline]
    pub unsafe fn unlock(&self) {
        let state = self.state.fetch_sub(LOCKED, Ordering::Release);
        if state != LOCKED {
            self.unlock_slow();
        }
    }

    #[cold]
    fn unlock_slow(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if (state & WAITING == 0) || (state & (LOCKED | WAKING) != 0) {
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
        loop {
            let (head, tail) = unsafe {
                let head = NonNull::new_unchecked((state & WAITING) as *mut Waiter);
                let tail = head.as_ref().tail.get().unwrap_or_else(|| {
                    let mut current = head;
                    loop {
                        let next = current.as_ref().next.get().unwrap();
                        next.as_ref().prev.set(Some(current));
                        current = next;
                        if let Some(tail) = current.as_ref().tail.get() {
                            head.as_ref().tail.set(Some(tail));
                            break tail;
                        }
                    }
                });
                (&*head.as_ptr(), &*tail.as_ptr())
            };

            if state & LOCKED != 0 {
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

            if let Some(new_tail) = tail.prev.get() {
                head.tail.set(Some(new_tail));
                self.state.fetch_and(!WAKING, Ordering::Release);
            } else if let Err(e) = self.state.compare_exchange_weak(
                state,
                UNLOCKED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                state = e;
                continue;
            }

            state = tail.state.load(Ordering::Relaxed);
            loop {
                match WakerState::from(state) {
                    WakerState::Empty => unreachable!("tried to wake a waiter without a Waker"),
                    WakerState::Stored => match tail.state.compare_exchange_weak(
                        WakerState::Stored as usize,
                        WakerState::Consuming as usize,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    ) {
                        Err(e) => state = e,
                        Ok(_) => {
                            let waker = tail.waker.replace(None).expect("waiter without a waker");
                            tail.state
                                .store(WakerState::Empty as usize, Ordering::Release);
                            waker.wake();
                            return;
                        }
                    },
                    WakerState::Updating => match tail.state.compare_exchange_weak(
                        WakerState::Updating as usize,
                        WakerState::Empty as usize,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Err(e) => state = e,
                        Ok(_) => return,
                    },
                    WakerState::Consuming => {
                        unreachable!("multiple threads trying to wake a waiter's Waker")
                    }
                }
            }
        }
    }
}

#[repr(usize)]
#[derive(Debug)]
enum WakerState {
    Empty = 0,
    Stored = 1,
    Updating = 2,
    Consuming = 3,
}

impl From<usize> for WakerState {
    fn from(value: usize) -> Self {
        match value & 0b11 {
            0 => Self::Empty,
            1 => Self::Stored,
            2 => Self::Updating,
            3 => Self::Consuming,
            _ => unreachable!(),
        }
    }
}

#[repr(align(4))]
struct Waiter {
    prev: Cell<Option<NonNull<Self>>>,
    next: Cell<Option<NonNull<Self>>>,
    tail: Cell<Option<NonNull<Self>>>,
    waker: Cell<Option<Waker>>,
    state: AtomicUsize,
}

enum LockState<'a, T> {
    LockFast(&'a Lock<T>),
    LockSlow(&'a Lock<T>),
    Locked,
}

pub struct LockFuture<'a, T> {
    waiter: UnsafeCell<Waiter>,
    state: LockState<'a, T>,
    _pinned: PhantomPinned,
}

impl<'a, T> Drop for LockFuture<'a, T> {
    fn drop(&mut self) {
        if let LockState::LockSlow(_) = self.state {
            unreachable!("LockFuture does not support cancellation");
        }
    }
}

impl<'a, T> Future for LockFuture<'a, T> {
    type Output = LockGuard<'a, T>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut_self = unsafe { Pin::get_unchecked_mut(self) };

        let guard = loop {
            match mut_self.state {
                LockState::LockFast(lock) => match lock.lock_fast() {
                    Some(guard) => break guard,
                    None => mut_self.state = LockState::LockSlow(lock),
                },
                LockState::LockSlow(lock) => {
                    let waiter = unsafe { &*mut_self.waiter.get() };
                    match Self::poll_lock(ctx, lock, waiter) {
                        Poll::Ready(guard) => break guard,
                        Poll::Pending => return Poll::Pending,
                    }
                }
                LockState::Locked => unreachable!("LockFuture polled after completion"),
            }
        };

        mut_self.state = LockState::Locked;
        Poll::Ready(guard)
    }
}

impl<'a, T> LockFuture<'a, T> {
    fn new(state: LockState<'a, T>) -> Self {
        Self {
            waiter: UnsafeCell::new(Waiter {
                prev: Cell::new(None),
                next: Cell::new(None),
                tail: Cell::new(None),
                waker: Cell::new(None),
                state: AtomicUsize::new(WakerState::Empty as usize),
            }),
            state,
            _pinned: PhantomPinned,
        }
    }

    fn poll_lock(
        ctx: &mut Context<'_>,
        lock: &'a Lock<T>,
        waiter: &Waiter,
    ) -> Poll<LockGuard<'a, T>> {
        let state = waiter.state.load(Ordering::Relaxed);
        match WakerState::from(state) {
            WakerState::Empty => {}
            WakerState::Stored => match waiter.state.compare_exchange(
                WakerState::Stored as usize,
                WakerState::Updating as usize,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    waiter.waker.set(Some(ctx.waker().clone()));
                    match waiter.state.compare_exchange(
                        WakerState::Updating as usize,
                        WakerState::Stored as usize,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => return Poll::Pending,
                        Err(state) => match WakerState::from(state) {
                            WakerState::Empty => {}
                            WakerState::Stored => unreachable!("another thread updated the Waker"),
                            WakerState::Updating => unreachable!(),
                            WakerState::Consuming => {
                                unreachable!("waker thread consuming Waker while we're updating it")
                            }
                        },
                    }
                }
                Err(state) => match WakerState::from(state) {
                    WakerState::Empty => {}
                    WakerState::Stored => unreachable!(),
                    WakerState::Updating => unreachable!("another thread is updating the Waker"),
                    WakerState::Consuming => return Poll::Pending,
                },
            },
            WakerState::Updating => unreachable!("another thread is updating the Waker"),
            WakerState::Consuming => return Poll::Pending,
        }

        let mut has_waker = false;
        let mut spin_wait = SpinWait::new();
        let mut state = lock.state.load(Ordering::Relaxed);

        loop {
            let new_state;
            let head = NonNull::new((state & WAITING) as *mut Waiter);

            if state & LOCKED == 0 {
                new_state = state | LOCKED;
            } else if head.is_none() && spin_wait.yield_now() {
                state = lock.state.load(Ordering::Relaxed);
                continue;
            } else {
                new_state = (state & !WAITING) | (waiter as *const _ as usize);
                waiter.prev.set(None);
                waiter.next.set(head);
                waiter.tail.set(match head {
                    None => Some(NonNull::from(waiter)),
                    Some(_) => None,
                });

                if !has_waker {
                    has_waker = true;
                    waiter.waker.set(Some(ctx.waker().clone()));
                    waiter
                        .state
                        .store(WakerState::Stored as usize, Ordering::Release);
                }
            }

            match lock.state.compare_exchange_weak(
                state,
                new_state,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) if state & LOCKED == 0 => return Poll::Ready(LockGuard(lock)),
                Ok(_) => return Poll::Pending,
                Err(e) => state = e,
            }
        }
    }
}

pub struct LockGuard<'a, T>(&'a Lock<T>);

impl<'a, T> Drop for LockGuard<'a, T> {
    fn drop(&mut self) {
        unsafe { self.0.unlock() }
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

impl<'a, T: fmt::Debug> fmt::Debug for LockGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (&&*self).fmt(f)
    }
}
