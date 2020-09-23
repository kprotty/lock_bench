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

use crate::{
    base::{List, Node, ParkWaker, RawLock as Lock, StaticInit},
    thread_parker::ThreadParker as Parker,
};
use core::{
    cell::UnsafeCell,
    future::Future,
    marker::PhantomPinned,
    mem::{forget, replace},
    ops::{Deref, DerefMut},
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
    time::Duration,
};

enum WaitState<P: Parker> {
    Empty,
    Waiting(Waker, P::Instant),
    Notified,
    Acquired,
}

type Waiter<P> = Node<WaitState<P>>;
type WaiterQueue<P> = List<WaitState<P>>;

const UNLOCKED: usize = 0;
const LOCKED: usize = 1;
const PARKED: usize = 2;

pub struct Mutex<P: Parker, T> {
    state: AtomicUsize,
    value: UnsafeCell<T>,
    queue: Lock<P, WaiterQueue<P>>,
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

impl<P: Parker, T: StaticInit> Mutex<P, T> {
    pub const INIT: Self = Self {
        state: AtomicUsize::new(UNLOCKED),
        value: UnsafeCell::new(T::INIT),
        queue: Lock::new(WaiterQueue::new()),
    };
}

impl<P: Parker, T> Mutex<P, T> {
    pub fn new(value: T) -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED),
            value: UnsafeCell::new(value),
            queue: Lock::new(WaiterQueue::new()),
        }
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    fn with_queue<F>(&self, f: impl FnOnce(&mut WaiterQueue<P>) -> F) -> F {
        f(&mut *self.queue.lock())
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
    fn lock_fast(&self) -> Option<MutexGuard<'_, P, T>> {
        self.state
            .compare_exchange_weak(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| MutexGuard(self))
    }

    pub fn lock_async(&self) -> MutexLockFuture<'_, P, T> {
        MutexLockFuture::new(match self.lock_fast() {
            Some(guard) => LockState::Locked(guard),
            None => LockState::Locking(self),
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
    fn lock_sync(&self, try_park: impl Fn(&P) -> bool) -> Option<MutexGuard<'_, P, T>> {
        self.lock_fast().or_else(|| self.lock_slow(try_park))
    }

    #[cold]
    fn lock_slow(&self, try_park: impl Fn(&P) -> bool) -> Option<MutexGuard<'_, P, T>> {
        let parker = P::new();
        let waker = unsafe { ParkWaker::<P>::from(&parker) };

        let mut context = Context::from_waker(&waker);
        let mut future = MutexLockFuture::new(LockState::Locking(self));

        loop {
            let pinned_future = unsafe { Pin::new_unchecked(&mut future) };
            match pinned_future.poll(&mut context) {
                Poll::Ready(guard) => return Some(guard),
                Poll::Pending => {
                    if try_park(&parker) {
                        continue;
                    }

                    if !future.cancel(self) {
                        parker.park();
                    }

                    forget(future);
                    return None;
                }
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
            self.unlock_slow(force_fair);
        }
    }

    #[cold]
    fn unlock_slow(&self, force_fair: bool) {
        if let Some(waker) =
            self.with_queue(|queue| unsafe { self.unlock_inner(queue, force_fair) })
        {
            waker.wake()
        }
    }

    unsafe fn unlock_inner(&self, queue: &mut WaiterQueue<P>, force_fair: bool) -> Option<Waker> {
        queue
            .pop_front()
            .map(|waiter| {
                let waiter = waiter.as_ref();
                let wait_state = &mut *waiter.get();

                let (waker, force_fair_at) = match replace(wait_state, WaitState::Notified) {
                    WaitState::Empty => unreachable!("empty wait state when dequeued"),
                    WaitState::Waiting(waker, force_fair_at) => (waker, force_fair_at),
                    WaitState::Notified => {
                        unreachable!("wait state already notified when dequeued")
                    }
                    WaitState::Acquired => {
                        unreachable!("wait state already acquired when dequeued")
                    }
                };

                if force_fair || (P::now() >= force_fair_at) {
                    *wait_state = WaitState::Acquired;
                    if queue.is_empty() {
                        self.state.store(LOCKED, Ordering::Relaxed);
                    }
                } else if queue.is_empty() {
                    self.state.store(UNLOCKED, Ordering::Release);
                } else {
                    self.state.store(PARKED, Ordering::Release);
                }

                Some(waker)
            })
            .unwrap_or_else(|| {
                self.state.store(UNLOCKED, Ordering::Release);
                None
            })
    }
}

enum LockState<'a, P: Parker, T> {
    Locking(&'a Mutex<P, T>),
    Waiting(&'a Mutex<P, T>),
    Locked(MutexGuard<'a, P, T>),
    Completed,
}

pub struct MutexLockFuture<'a, P: Parker, T> {
    state: LockState<'a, P, T>,
    waiter: Waiter<P>,
    _pinned: PhantomPinned,
}

unsafe impl<'a, P: Parker, T: Send> Send for MutexLockFuture<'a, P, T> {}

impl<'a, P: Parker, T> Drop for MutexLockFuture<'a, P, T> {
    fn drop(&mut self) {
        if let LockState::Waiting(mutex) = self.state {
            let _ = self.cancel(mutex);
        }
    }
}

impl<'a, P: Parker, T> Future for MutexLockFuture<'a, P, T> {
    type Output = MutexGuard<'a, P, T>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut_self = unsafe { Pin::get_unchecked_mut(self) };
        loop {
            match &mut_self.state {
                LockState::Locking(mutex) => match mut_self.poll_lock(ctx, mutex) {
                    Poll::Ready(guard) => mut_self.state = LockState::Locked(guard),
                    Poll::Pending => {
                        mut_self.state = LockState::Waiting(mutex);
                        return Poll::Pending;
                    }
                },
                LockState::Waiting(mutex) => match mut_self.poll_wait(ctx, mutex) {
                    Poll::Ready(Some(guard)) => mut_self.state = LockState::Locked(guard),
                    Poll::Ready(None) => mut_self.state = LockState::Locking(mutex),
                    Poll::Pending => return Poll::Pending,
                },
                LockState::Locked(_) => match replace(&mut mut_self.state, LockState::Completed) {
                    LockState::Locked(guard) => return Poll::Ready(guard),
                    _ => unreachable!(),
                },
                LockState::Completed => {
                    unreachable!("MutexLockFuture polled after completion");
                }
            }
        }
    }
}

impl<'a, P: Parker, T> MutexLockFuture<'a, P, T> {
    fn new(state: LockState<'a, P, T>) -> Self {
        Self {
            state,
            waiter: Waiter::new(WaitState::Empty),
            _pinned: PhantomPinned,
        }
    }

    #[cold]
    fn poll_lock(&self, ctx: &Context<'_>, mutex: &'a Mutex<P, T>) -> Poll<MutexGuard<'a, P, T>> {
        let mut spin = 0;
        let waiter = &self.waiter;
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
                if P::yield_now(spin) {
                    spin = spin.wrapping_add(1);
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

            if mutex.with_queue(|queue| unsafe {
                let state = mutex.state.load(Ordering::Relaxed);
                if state != (LOCKED | PARKED) {
                    return false;
                }

                let timeout_ns = 1_000_000;
                *waiter.get() =
                    WaitState::Waiting(ctx.waker().clone(), P::now() + Duration::new(0, timeout_ns));

                queue.push_back(NonNull::from(waiter));
                true
            }) {
                return Poll::Pending;
            }

            spin = 0;
            state = mutex.state.load(Ordering::Relaxed);
        }
    }

    #[cold]
    fn poll_wait(
        &self,
        ctx: &mut Context<'_>,
        mutex: &'a Mutex<P, T>,
    ) -> Poll<Option<MutexGuard<'a, P, T>>> {
        match mutex.with_queue(|_| unsafe {
            let wait_state = &mut *self.waiter.get();
            match wait_state {
                WaitState::Empty => unreachable!("empty wait state when polling"),
                WaitState::Waiting(ref mut waker, _) => {
                    if !ctx.waker().will_wake(waker) {
                        *waker = ctx.waker().clone();
                    }
                    None
                }
                WaitState::Notified => Some(false),
                WaitState::Acquired => Some(true),
            }
        }) {
            None => Poll::Pending,
            Some(false) => Poll::Ready(None),
            Some(true) => Poll::Ready(Some(MutexGuard(mutex))),
        }
    }

    #[cold]
    fn cancel(&self, mutex: &'a Mutex<P, T>) -> bool {
        match mutex.with_queue(|queue| unsafe {
            let wait_state = &*self.waiter.get();
            match wait_state {
                WaitState::Empty => unreachable!("empty waiter when cancelling"),
                WaitState::Waiting(_, _) => {
                    assert!(queue.try_remove(NonNull::from(&self.waiter)));
                    None
                }
                WaitState::Notified => Some(None),
                WaitState::Acquired => Some(mutex.unlock_inner(queue, false)),
            }
        }) {
            None => true,
            Some(waker) => {
                if let Some(waker) = waker {
                    waker.wake();
                }
                false
            }
        }
    }
}

pub struct MutexGuard<'a, P: Parker, T>(&'a Mutex<P, T>);

impl<'a, P: Parker, T> Drop for MutexGuard<'a, P, T> {
    fn drop(&mut self) {
        unsafe { self.0.unlock() }
    }
}

impl<'a, P: Parker, T> DerefMut for MutexGuard<'a, P, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.0.value.get() }
    }
}

impl<'a, P: Parker, T> Deref for MutexGuard<'a, P, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.0.value.get() }
    }
}
