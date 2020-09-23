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
    base::{Node, ParkWaker},
    thread_parker::ThreadParker as Parker,
};
use core::{
    cell::{Cell, UnsafeCell},
    convert::TryInto,
    future::Future,
    marker::{PhantomData, PhantomPinned},
    mem::replace,
    ops::{Deref, DerefMut},
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
    time::Duration,
};

const UNLOCKED: usize = 0;
const LOCKED: usize = 1;
const WAITING: usize = !LOCKED;

pub struct Lock<P, T> {
    state: AtomicUsize,
    value: UnsafeCell<T>,
    _phantom: PhantomData<P>,
}

unsafe impl<P, T: Send> Send for Lock<P, T> {}
unsafe impl<P, T: Send> Sync for Lock<P, T> {}

impl<P, T: Default> Default for Lock<P, T> {
    fn default() -> Self {
        Self::from(T::default())
    }
}

impl<P, T> From<T> for Lock<P, T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<P, T> AsMut<T> for Lock<P, T> {
    fn as_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }
}

impl<P, T> Lock<P, T> {
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED),
            value: UnsafeCell::new(value),
            _phantom: PhantomData,
        }
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }
}

impl<P: Parker, T> Lock<P, T> {
    #[inline]
    pub fn try_lock(&self) -> Option<LockGuard<'_, P, T>> {
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
    fn lock_fast(&self) -> Option<LockGuard<'_, P, T>> {
        self.state
            .compare_exchange_weak(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| LockGuard(self))
    }

    pub fn lock_async(&self) -> LockFuture<'_, P, T> {
        LockFuture::new(match self.lock_fast() {
            Some(guard) => LockState::Locked(guard),
            None => LockState::Locking(self),
        })
    }

    #[inline]
    pub fn lock(&self) -> LockGuard<'_, P, T> {
        self.lock_fast().unwrap_or_else(|| self.lock_slow())
    }

    #[cold]
    fn lock_slow(&self) -> LockGuard<'_, P, T> {
        let parker = P::new();
        let waker = unsafe { ParkWaker::<P>::from(&parker) };

        let mut context = Context::from_waker(&waker);
        let mut future = LockFuture::new(LockState::Locking(self));

        loop {
            let pinned_future = unsafe { Pin::new_unchecked(&mut future) };
            match pinned_future.poll(&mut context) {
                Poll::Pending => parker.park(),
                Poll::Ready(guard) => return guard,
            }
        }
    }

    #[inline]
    pub unsafe fn unlock(&self) {
        if self
            .state
            .compare_exchange(LOCKED, UNLOCKED, Ordering::Release, Ordering::Relaxed)
            .is_err()
        {
            self.unlock_slow();
        }
    }

    #[cold]
    unsafe fn unlock_slow(&self) {
        let mut is_fair = None;
        let mut state = self.state.load(Ordering::Acquire);

        loop {
            let head =
                NonNull::new((state & WAITING) as *mut Waiter<P>).expect("unlock without a waiter");
            let tail = {
                let mut current = head;
                loop {
                    if let Some(tail) = current.as_ref().tail.get() {
                        head.as_ref().tail.set(Some(tail));
                        break tail;
                    } else {
                        let next = current.as_ref().next.get();
                        let next = next.expect("waiter next link not set while enqueued");
                        next.as_ref().prev.set(Some(current));
                        current = next;
                    }
                }
            };

            let wait_state = &*tail.as_ref().get();
            let be_fair = is_fair.unwrap_or_else(|| {
                let force_fair_at = wait_state
                    .force_fair_at
                    .get()
                    .expect("waiter without a starting point set");
                is_fair = Some(P::now() >= force_fair_at);
                is_fair.unwrap()
            });

            if let Some(new_tail) = tail.as_ref().prev.get() {
                head.as_ref().tail.set(Some(new_tail));
                if !be_fair {
                    self.state.fetch_and(!LOCKED, Ordering::Release);
                }
            } else if let Err(e) = self.state.compare_exchange_weak(
                state,
                if be_fair { LOCKED } else { UNLOCKED },
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                state = e;
                continue;
            }

            let new_state = if be_fair {
                WAKER_ACQUIRED
            } else {
                WAKER_NOTIFIED
            };
            state = wait_state.state.load(Ordering::Relaxed);
            loop {
                match state {
                    WAKER_WAITING => match wait_state.state.compare_exchange_weak(
                        WAKER_WAITING,
                        WAKER_CONSUMING,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    ) {
                        Err(e) => state = e,
                        Ok(_) => {
                            let waker = wait_state
                                .waker
                                .replace(None)
                                .expect("wait state without a waker");
                            wait_state.state.store(new_state, Ordering::Release);
                            return waker.wake();
                        }
                    },
                    WAKER_UPDATING => match wait_state.state.compare_exchange_weak(
                        WAKER_UPDATING,
                        new_state,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Err(e) => state = e,
                        Ok(_) => return,
                    },
                    _ => unreachable!("invalid Lock wait state {:?} when waking", state),
                }
            }
        }
    }
}

enum LockState<'a, P: Parker, T> {
    Locking(&'a Lock<P, T>),
    Waiting(&'a Lock<P, T>),
    Locked(LockGuard<'a, P, T>),
    Completed,
}

const WAKER_WAITING: usize = 0;
const WAKER_UPDATING: usize = 1;
const WAKER_CONSUMING: usize = 2;
const WAKER_NOTIFIED: usize = 3;
const WAKER_ACQUIRED: usize = 4;

struct WaitState<P: Parker> {
    state: AtomicUsize,
    waker: Cell<Option<Waker>>,
    force_fair_at: Cell<Option<P::Instant>>,
}

type Waiter<P> = Node<WaitState<P>>;

pub struct LockFuture<'a, P: Parker, T> {
    state: LockState<'a, P, T>,
    waiter: Waiter<P>,
    _pinned: PhantomPinned,
}

unsafe impl<'a, P: Parker, T: Send> Send for LockFuture<'a, P, T> {}

impl<'a, P: Parker, T> Drop for LockFuture<'a, P, T> {
    fn drop(&mut self) {
        if let LockState::Waiting(_) = self.state {
            unreachable!("LockFuture does not support cancellation");
        }
    }
}

impl<'a, P: Parker, T> Future for LockFuture<'a, P, T> {
    type Output = LockGuard<'a, P, T>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut_self = unsafe { Pin::get_unchecked_mut(self) };
        loop {
            match &mut_self.state {
                LockState::Locking(lock) => match mut_self.poll_lock(ctx, lock) {
                    Poll::Ready(guard) => mut_self.state = LockState::Locked(guard),
                    Poll::Pending => {
                        mut_self.state = LockState::Waiting(lock);
                        return Poll::Pending;
                    }
                },
                LockState::Waiting(lock) => match mut_self.poll_wait(ctx, lock) {
                    Poll::Ready(Some(guard)) => mut_self.state = LockState::Locked(guard),
                    Poll::Ready(None) => mut_self.state = LockState::Locking(lock),
                    Poll::Pending => return Poll::Pending,
                },
                LockState::Locked(_) => match replace(&mut mut_self.state, LockState::Completed) {
                    LockState::Locked(guard) => return Poll::Ready(guard),
                    _ => unreachable!(),
                },
                LockState::Completed => {
                    unreachable!("LockFuture polled after completion");
                }
            }
        }
    }
}

impl<'a, P: Parker, T> LockFuture<'a, P, T> {
    fn new(state: LockState<'a, P, T>) -> Self {
        Self {
            state,
            waiter: Waiter::new(WaitState {
                state: AtomicUsize::new(WAKER_WAITING),
                waker: Cell::new(None),
                force_fair_at: Cell::new(None),
            }),
            _pinned: PhantomPinned,
        }
    }

    #[cold]
    fn poll_lock(&self, ctx: &Context<'_>, lock: &'a Lock<P, T>) -> Poll<LockGuard<'a, P, T>> {
        let mut spin = 0;
        let waiter = &self.waiter;
        let wait_state = unsafe { &*waiter.get() };
        let mut state = lock.state.load(Ordering::Relaxed);

        loop {
            let new_state;
            let head = NonNull::new((state & WAITING) as *mut Waiter<P>);

            if state & LOCKED == 0 {
                new_state = state | LOCKED;
            } else if head.is_none() && P::yield_now(spin) {
                spin = spin.wrapping_add(1);
                state = lock.state.load(Ordering::Relaxed);
                continue;
            } else {
                new_state = (state & !WAITING) | (waiter as *const _ as usize);
                waiter.prev.set(None);
                waiter.next.set(head);
                waiter.tail.set(match head {
                    Some(_) => None,
                    None => Some(NonNull::from(waiter)),
                });

                unsafe {
                    if (&*wait_state.waker.as_ptr()).is_none() {
                        wait_state.waker.set(Some(ctx.waker().clone()));
                        wait_state.state.store(WAKER_WAITING, Ordering::Relaxed);
                    }

                    if (&*wait_state.force_fair_at.as_ptr()).is_none() {
                        let timeout_ns = {
                            let ptr = head.unwrap_or(NonNull::from(waiter));
                            let ptr = ptr.as_ptr() as usize;
                            let ptr = ((ptr * 13) ^ (ptr >> 15)) & (!0u32 as usize);
                            let rng_u32: u32 = ptr.try_into().unwrap();
                            rng_u32 % 1_000_000
                        };

                        wait_state
                            .force_fair_at
                            .set(Some(P::now() + Duration::new(0, timeout_ns)));
                    }
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

    #[cold]
    fn poll_wait(
        &self,
        ctx: &Context<'_>,
        lock: &'a Lock<P, T>,
    ) -> Poll<Option<LockGuard<'a, P, T>>> {
        let wait_state = unsafe { &*self.waiter.get() };

        let mut state = wait_state.state.load(Ordering::Relaxed);
        loop {
            match state {
                WAKER_WAITING => match wait_state.state.compare_exchange_weak(
                    WAKER_WAITING,
                    WAKER_UPDATING,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Err(e) => state = e,
                    Ok(_) => {
                        wait_state.waker.set(Some(ctx.waker().clone()));
                        match wait_state.state.compare_exchange(
                            WAKER_UPDATING,
                            WAKER_WAITING,
                            Ordering::Release,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => return Poll::Pending,
                            Err(e) => {
                                state = e;
                                match state {
                                    WAKER_NOTIFIED | WAKER_ACQUIRED => {}
                                    _ => unreachable!(
                                        "invalid Lock wait state when updating {:?}",
                                        state
                                    ),
                                }
                            }
                        }
                    }
                },
                WAKER_CONSUMING => return Poll::Pending,
                WAKER_NOTIFIED => return Poll::Ready(None),
                WAKER_ACQUIRED => return Poll::Ready(Some(LockGuard(lock))),
                _ => unreachable!("invalid Lock wait state {:?} when polling", state),
            }
        }
    }
}

pub struct LockGuard<'a, P: Parker, T>(&'a Lock<P, T>);

impl<'a, P: Parker, T> Drop for LockGuard<'a, P, T> {
    fn drop(&mut self) {
        unsafe { self.0.unlock() }
    }
}

impl<'a, P: Parker, T> DerefMut for LockGuard<'a, P, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.0.value.get() }
    }
}

impl<'a, P: Parker, T> Deref for LockGuard<'a, P, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.0.value.get() }
    }
}
