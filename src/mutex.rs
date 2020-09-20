use super::{
    list::{List, Node},
    lock::Lock,
    SpinWait, ThreadParkerTimed as Parker,
};
use core::{
    cell::UnsafeCell,
    convert::TryInto,
    fmt,
    future::Future,
    marker::PhantomPinned,
    num::NonZeroU16,
    ops::{Deref, DerefMut},
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
    time::Duration,
};

struct Waiting<P: Parker> {
    waker: Waker,
    prng: NonZeroU16,
    force_fair_at: P::Instant,
}

#[derive(Debug)]
enum Notified {
    Retry,
    Acquired,
}

enum WaitState<P: Parker> {
    Empty,
    Waiting(Waiting<P>),
    Notified(Notified),
}

impl<P: Parker> fmt::Debug for WaitState<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WaitState::Empty => write!(f, "WaitState::Empty"),
            WaitState::Waiting(_) => write!(f, "WaitState::Waiting"),
            WaitState::Notified(notified) => write!(f, "WaitState::{:?}", notified),
        }
    }
}

pub struct Mutex<P: Parker, T> {
    state: AtomicUsize,
    value: UnsafeCell<T>,
    queue: Lock<List<WaitState<P>>>,
}

unsafe impl<P: Parker, T: Send> Send for Mutex<P, T> {}
unsafe impl<P: Parker, T: Send> Sync for Mutex<P, T> {}

impl<P: Parker, T: Default> Default for Mutex<P, T> {
    fn default() -> Self {
        Self::from(T::default())
    }
}

impl<P: Parker, T> From<T> for Mutex<P, T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<P: Parker, T> AsMut<T> for Mutex<P, T> {
    fn as_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }
}

impl<P: Parker, T: fmt::Debug> fmt::Debug for Mutex<P, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut f = f.debug_struct("Mutex");
        match self.try_lock() {
            Some(guard) => f.field("value", &&*guard),
            None => f.field("state", &"<locked>"),
        }
        .finish()
    }
}

const UNLOCKED: usize = 0;
const LOCKED: usize = 1;
const PARKED: usize = 2;
const PRNG_SHIFT: u32 = (LOCKED | PARKED).count_ones();

impl<P: Parker, T> Mutex<P, T> {
    pub fn new(value: T) -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED),
            value: UnsafeCell::new(value),
            queue: Lock::new(List::new()),
        }
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    #[inline]
    fn with_queue<F>(&self, f: impl FnOnce(&mut List<WaitState<P>>) -> F) -> F {
        f(&mut *self.queue.lock::<P>())
    }

    #[inline]
    pub fn is_locked(&self) -> bool {
        let state = self.state.load(Ordering::Relaxed);
        state & LOCKED != 0
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
    pub fn lock(&self) -> MutexGuard<'_, P, T> {
        self.lock_sync(|parker| {
            parker.park();
            true
        })
        .unwrap()
    }

    #[inline]
    pub fn try_lock_for(&self, timeout: Duration) -> Option<MutexGuard<'_, P, T>> {
        self.try_lock_until(P::now() + timeout)
    }

    #[inline]
    pub fn try_lock_until(&self, deadline: P::Instant) -> Option<MutexGuard<'_, P, T>> {
        self.lock_sync(|parker| {
            parker.park_until(deadline);
            P::now() >= deadline
        })
    }

    #[inline]
    pub fn lock_async(&self) -> MutexLockFuture<'_, P, T> {
        MutexLockFuture::new(LockState::LockFast(self))
    }

    #[inline]
    fn lock_sync(&self, try_park: impl Fn(&P) -> bool) -> Option<MutexGuard<'_, P, T>> {
        self.lock_fast().or_else(|| self.lock_slow(try_park))
    }

    #[inline]
    fn lock_fast(&self) -> Option<MutexGuard<'_, P, T>> {
        self.state
            .compare_exchange_weak(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| MutexGuard(self))
    }

    const VTABLE: RawWakerVTable = RawWakerVTable::new(
        |ptr| unsafe {
            (&*(ptr as *const P)).prepare_park();
            RawWaker::new(ptr, &Self::VTABLE)
        },
        |ptr| unsafe { (&*(ptr as *const P)).unpark() },
        |ptr| unsafe { (&*(ptr as *const P)).unpark() },
        |_ptr| {},
    );

    #[cold]
    fn lock_slow(&self, try_park: impl Fn(&P) -> bool) -> Option<MutexGuard<'_, P, T>> {
        let parker = P::new();
        let waker = unsafe {
            let ptr = &parker as *const _ as *const ();
            let raw_waker = RawWaker::new(ptr, &Self::VTABLE);
            Waker::from_raw(raw_waker)
        };

        let mut context = Context::from_waker(&waker);
        let mut future = MutexLockFuture::new(LockState::LockSlow(self));

        loop {
            let pinned_future = unsafe { Pin::new_unchecked(&mut future) };
            match pinned_future.poll(&mut context) {
                Poll::Ready(guard) => return Some(guard),
                Poll::Pending => match try_park(&parker) {
                    true => continue,
                    false => return None,
                },
            }
        }
    }

    #[inline]
    pub unsafe fn unlock(&self) {
        self.unlock_fast(false)
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
            self.unlock_slow(force_fair)
        }
    }

    #[cold]
    fn unlock_slow(&self, force_fair: bool) {
        if let Some(waker) = self.with_queue(|queue| unsafe {
            let (waker, new_state) = queue
                .pop_front()
                .map(|waiter| {
                    let wait_state = &mut *waiter.as_ref().get();
                    let Waiting {
                        prng,
                        waker,
                        force_fair_at,
                    } = match std::mem::replace(wait_state, WaitState::Notified(Notified::Retry)) {
                        WaitState::Empty => unreachable!("trying to wake a waiter without a Waker"),
                        WaitState::Waiting(waiting) => waiting,
                        WaitState::Notified(_) => {
                            unreachable!("notified waiter still in waiting queue")
                        }
                    };

                    let new_state = if false || force_fair || P::now() >= force_fair_at {
                        *wait_state = WaitState::Notified(Notified::Acquired);
                        if queue.is_empty() {
                            Some(LOCKED)
                        } else {
                            None
                        }
                    } else if queue.is_empty() {
                        Some(UNLOCKED)
                    } else {
                        Some(PARKED)
                    };

                    (Some(waker), new_state.map(|new_state| (prng, new_state)))
                })
                .unwrap_or_else(|| {
                    let prng = self.state.load(Ordering::Relaxed);
                    let prng = (prng >> PRNG_SHIFT) & 0xffff;
                    let prng = NonZeroU16::new(prng.try_into().unwrap()).unwrap();
                    (None, Some((prng, UNLOCKED)))
                });

            if let Some((prng, new_state)) = new_state {
                let prng = (prng.get() as usize) << PRNG_SHIFT;
                self.state.store(prng | new_state, Ordering::Release);
            }

            waker
        }) {
            waker.wake()
        }
    }
}

enum LockState<'a, P: Parker, T> {
    LockFast(&'a Mutex<P, T>),
    LockSlow(&'a Mutex<P, T>),
    Waiting(&'a Mutex<P, T>),
    Locked,
}

pub struct MutexLockFuture<'a, P: Parker, T> {
    waiter: UnsafeCell<Node<WaitState<P>>>,
    state: LockState<'a, P, T>,
    _pinned: PhantomPinned,
}

impl<'a, P: Parker, T> Drop for MutexLockFuture<'a, P, T> {
    fn drop(&mut self) {
        if let LockState::Waiting(mutex) = self.state {
            Self::cancel(mutex, self.waiter());
        }
    }
}

impl<'a, P: Parker, T> Future for MutexLockFuture<'a, P, T> {
    type Output = MutexGuard<'a, P, T>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut_self = unsafe { Pin::get_unchecked_mut(self) };

        let guard = loop {
            match mut_self.state {
                LockState::LockFast(mutex) => match mutex.lock_fast() {
                    Some(guard) => break guard,
                    None => mut_self.state = LockState::LockSlow(mutex),
                },
                LockState::LockSlow(mutex) => {
                    mut_self.state = LockState::Waiting(mutex);
                    match Self::poll_lock(ctx, mutex, mut_self.waiter()) {
                        Poll::Ready(guard) => break guard,
                        Poll::Pending => return Poll::Pending,
                    }
                }
                LockState::Waiting(mutex) => match Self::poll_wait(ctx, mutex, mut_self.waiter()) {
                    Poll::Ready(Some(guard)) => break guard,
                    Poll::Ready(None) => mut_self.state = LockState::LockSlow(mutex),
                    Poll::Pending => return Poll::Pending,
                },
                LockState::Locked => unreachable!("MutexLockFuture polled after completion"),
            }
        };

        mut_self.state = LockState::Locked;
        Poll::Ready(guard)
    }
}

impl<'a, P: Parker, T> MutexLockFuture<'a, P, T> {
    fn new(state: LockState<'a, P, T>) -> Self {
        Self {
            waiter: UnsafeCell::new(Node::new(WaitState::Empty)),
            state,
            _pinned: PhantomPinned,
        }
    }

    fn waiter(&self) -> &Node<WaitState<P>> {
        unsafe { &*self.waiter.get() }
    }

    fn poll_lock(
        ctx: &mut Context<'_>,
        mutex: &'a Mutex<P, T>,
        waiter: &Node<WaitState<P>>,
    ) -> Poll<MutexGuard<'a, P, T>> {
        let mut spin_wait = SpinWait::new();
        let mut state = mutex.state.load(Ordering::Relaxed);

        loop {
            if state & LOCKED == 0 {
                match mutex.state.compare_exchange_weak(
                    state,
                    state | LOCKED,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return Poll::Ready(MutexGuard(mutex)),
                    Err(e) => state = e,
                }
                continue;
            }

            if state & PARKED == 0 {
                if spin_wait.yield_now() {
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
                let state = mutex.state.load(Ordering::Relaxed);
                if state & (LOCKED | PARKED) != (LOCKED | PARKED) {
                    return Poll::Ready(());
                }

                let mut prng = queue
                    .tail()
                    .map(|last_waiter| match &*last_waiter.as_ref().get() {
                        WaitState::Waiting(waiting) => waiting.prng,
                        wait_state => {
                            unreachable!("last waiter with invalid wait state {:?}", wait_state)
                        }
                    })
                    .or_else(|| {
                        let prng = (state >> PRNG_SHIFT) & 0xffff;
                        NonZeroU16::new(prng.try_into().unwrap())
                    })
                    .unwrap_or_else(|| {
                        let waiter_ptr = waiter as *const _ as usize;
                        let state_ptr = &mutex.state as *const _ as usize;
                        let seed = ((waiter_ptr ^ state_ptr) >> 8) & 0xffff;
                        NonZeroU16::new(seed.try_into().unwrap()).unwrap()
                    });

                let mut xorshift16 = || {
                    let mut xs = prng.get();
                    xs ^= xs << 7;
                    xs ^= xs >> 9;
                    xs ^= xs << 8;
                    prng = NonZeroU16::new(xs).unwrap();
                    xs
                };

                let fair_timeout_ns = {
                    let low = xorshift16();
                    let high = xorshift16();
                    let rng_u32 = ((high as u32) << 16) | (low as u32);
                    rng_u32 % 10_000_000
                };

                let wait_state = &mut *waiter.get();
                *wait_state = WaitState::Waiting(Waiting {
                    prng,
                    waker: ctx.waker().clone(),
                    force_fair_at: P::now() + Duration::new(0, fair_timeout_ns),
                });

                queue.push_back(NonNull::from(waiter));
                Poll::Pending
            }) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(()) => {
                    spin_wait.reset();
                    state = mutex.state.load(Ordering::Relaxed);
                }
            }
        }
    }

    fn poll_wait(
        ctx: &mut Context<'_>,
        mutex: &'a Mutex<P, T>,
        waiter: &Node<WaitState<P>>,
    ) -> Poll<Option<MutexGuard<'a, P, T>>> {
        mutex.with_queue(|_queue| unsafe {
            let wait_state = &mut *waiter.get();
            match wait_state {
                WaitState::Empty => unreachable!("waiter without a Waker in the wait queue"),
                WaitState::Waiting(ref mut waiting) => {
                    waiting.waker = ctx.waker().clone();
                    Poll::Pending
                }
                WaitState::Notified(Notified::Retry) => Poll::Ready(None),
                WaitState::Notified(Notified::Acquired) => Poll::Ready(Some(MutexGuard(mutex))),
            }
        })
    }

    #[cold]
    fn cancel(mutex: &'a Mutex<P, T>, waiter: &Node<WaitState<P>>) {
        mutex.with_queue(|queue| unsafe {
            if !queue.try_remove(NonNull::from(waiter)) {
                return;
            }

            let wait_state = &mut *waiter.get();
            let waiting = match std::mem::replace(wait_state, WaitState::Empty) {
                WaitState::Waiting(waiting) => waiting,
                wait_state => {
                    unreachable!("waiter in queue with invalid wait state {:?}", wait_state)
                }
            };

            if queue.is_empty() {
                let state = mutex.state.load(Ordering::Relaxed);
                assert_ne!(
                    state & PARKED,
                    0,
                    "parked not set when wait queue not empty"
                );

                let mut new_state = (waiting.prng.get() as usize) << PRNG_SHIFT;
                new_state |= state & LOCKED;
                mutex.state.store(new_state, Ordering::Relaxed);
            }
        })
    }
}

pub struct MutexGuard<'a, P: Parker, T>(&'a Mutex<P, T>);

impl<'a, P: Parker, T> MutexGuard<'a, P, T> {
    pub fn unlock_fair(self: Self) {
        unsafe { self.0.unlock_fair() }
    }
}

impl<'a, P: Parker, T> Drop for MutexGuard<'a, P, T> {
    fn drop(&mut self) {
        unsafe { self.0.unlock() }
    }
}

impl<'a, P: Parker, T> Deref for MutexGuard<'a, P, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.0.value.get() }
    }
}

impl<'a, P: Parker, T> DerefMut for MutexGuard<'a, P, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.0.value.get() }
    }
}

impl<'a, P: Parker, T: fmt::Debug> fmt::Debug for MutexGuard<'a, P, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (&&*self).fmt(f)
    }
}
