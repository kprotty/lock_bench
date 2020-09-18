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
        self.0 ^= self.0 << 9;
        self.0 ^= self.0 << 8;
        self.0
    }

    fn gen_u64(&mut self) -> u64 {
        let a = self.xorshift16() as u64;
        let b = self.xorshift16() as u64;
        let c = self.xorshift16() as u64;
        let d = self.xorshift16() as u64;
        (a << 48) | (b << 32) | (c << 16) | d
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

pub struct RawMutex<P: ThreadParker, T> {
    state: AtomicU8,
    prng: AtomicU16,
    queue: Lock<List<WaitState<P>>>,
    value: UnsafeCell<T>,
}

impl<P: ThreadParker, T: fmt::Debug> fmt::Debug for RawMutex<P, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut f = f.debug_struct("RawMutex");
        let f = match self.try_lock() {
            Some(guard) => f.field("value", &&*guard),
            None => f.field("state", &"<locked>"),
        };
        f.finish()
    }
}

impl<P: ThreadParker, T: Default> Default for RawMutex<P, T> {
    fn default() -> Self {
        Self::from(T::default())
    }
}

impl<P: ThreadParker, T> From<T> for RawMutex<P, T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<P: ThreadParker, T> AsMut<T> for RawMutex<P, T> {
    fn as_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }
}

unsafe impl<P: ThreadParker, T: Send> Send for RawMutex<P, T> {}
unsafe impl<P: ThreadParker, T: Send> Sync for RawMutex<P, T> {}

const UNLOCKED: u8 = 0;
const LOCKED: u8 = 1 << 0;
const PARKED: u8 = 1 << 1;

impl<P: ThreadParker, T> RawMutex<P, T> {
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
    pub fn try_lock(&self) -> Option<RawMutexGuard<'_, P, T>> {
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
                Ok(_) => return Some(RawMutexGuard(self)),
                Err(e) => state = e,
            }
        }
    }

    #[inline]
    fn try_lock_fast(&self) -> Option<RawMutexGuard<'_, P, T>> {
        self.state
            .compare_exchange_weak(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| RawMutexGuard(self))
    }

    pub fn lock_async(&self) -> RawMutexFuture<'_, P, T> {
        RawMutexFuture {
            waiter: UnsafeCell::new(Node::new(WaitState::Empty)),
            mutex: MutexState::TryLock(self),
            _pinned: PhantomPinned,
        }
    }

    #[inline]
    pub fn lock(&self) -> RawMutexGuard<'_, P, T> {
        self.lock_sync(|parker| {
            parker.park();
            true
        })
        .unwrap()
    }

    #[inline]
    fn lock_sync<'a>(&'a self, try_park: impl Fn(&P) -> bool) -> Option<RawMutexGuard<'a, P, T>> {
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
    fn lock_sync_slow<'a>(
        &'a self,
        try_park: impl Fn(&P) -> bool,
    ) -> Option<RawMutexGuard<'a, P, T>> {
        let parker = P::new();
        let waker = unsafe {
            let parker_ptr = &parker as *const _ as *const ();
            let raw_waker = RawWaker::new(parker_ptr, &Self::VTABLE);
            Waker::from_raw(raw_waker)
        };

        let mut context = Context::from_waker(&waker);
        let mut future = RawMutexFuture {
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
    TryLock(&'a RawMutex<P, T>),
    Locking(&'a RawMutex<P, T>),
    Waiting(&'a RawMutex<P, T>),
    Locked,
}

pub struct RawMutexFuture<'a, P: ThreadParker, T> {
    waiter: UnsafeCell<Node<WaitState<P>>>,
    mutex: MutexState<'a, P, T>,
    _pinned: PhantomPinned,
}

impl<'a, P: ThreadParker, T> Future for RawMutexFuture<'a, P, T> {
    type Output = RawMutexGuard<'a, P, T>;

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
                    Some(Notified::Acquired) => break RawMutexGuard(mutex),
                    Some(Notified::Retry) => mut_self.mutex = MutexState::Locking(mutex),
                },
                MutexState::Locked => {
                    unreachable!("RawMutexFuture polled after completion");
                }
            }
        };

        mut_self.mutex = MutexState::Locked;
        Poll::Ready(guard)
    }
}

impl<'a, P: ThreadParker, T> Drop for RawMutexFuture<'a, P, T> {
    fn drop(&mut self) {
        match self.mutex {
            MutexState::Waiting(mutex) => self.poll_cancel(mutex),
            _ => {}
        }
    }
}

impl<'a, P: ThreadParker, T> RawMutexFuture<'a, P, T> {
    fn poll_lock(
        &self,
        ctx: &mut Context<'_>,
        mutex: &'a RawMutex<P, T>,
    ) -> Option<RawMutexGuard<'a, P, T>> {
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
                    Ok(_) => return Some(RawMutexGuard(mutex)),
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
                                let mut prng = Prng(mutex.prng.load(Ordering::Relaxed));
                                let rng_state = prng.gen_u64();
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

    fn poll_wait(&self, ctx: &mut Context<'_>, mutex: &'a RawMutex<P, T>) -> Option<Notified> {
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

    fn poll_cancel(&self, mutex: &'a RawMutex<P, T>) {
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

pub struct RawMutexGuard<'a, P: ThreadParker, T>(&'a RawMutex<P, T>);

impl<'a, P: ThreadParker, T> RawMutexGuard<'a, P, T> {
    pub fn unlock_fair(self: Self) {
        unsafe { self.0.unlock_fair() }
    }
}

impl<'a, P: ThreadParker, T> Drop for RawMutexGuard<'a, P, T> {
    fn drop(&mut self) {
        unsafe { self.0.unlock() }
    }
}

impl<'a, P: ThreadParker, T: fmt::Debug> fmt::Debug for RawMutexGuard<'a, P, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(f)
    }
}

impl<'a, P: ThreadParker, T> Deref for RawMutexGuard<'a, P, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.0.value.get() }
    }
}

impl<'a, P: ThreadParker, T> DerefMut for RawMutexGuard<'a, P, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.0.value.get() }
    }
}
