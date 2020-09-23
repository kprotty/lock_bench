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

use super::{
    super::ThreadParker as Parker,
    list::Node,
    atomic_waker::AtomicWaker,
};
use core::{
    time::Duration,
    convert::TryInto,
    cell::{Cell, UnsafeCell},
    future::Future,
    ptr::NonNull,
    marker::{PhantomData, PhantomPinned},
    ops::{Deref, DerefMut},
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

const UNLOCKED: usize = 0;
const LOCKED: usize = 1;
const WAITING: usize = !LOCKED;

const WAKER_NOTIFIED: usize = 0;
const WAKER_ACQUIRED: usize = 1;

pub struct Lock<P, T> {
    state: AtomicUsize,
    value: UnsafeCell<T>,
    _phantom: PhantomData<P>
}

unsafe impl<P, T: Send> Send for Lock<P, T> {}
unsafe impl<P, T: Send> Sync for Lock<P, T> {}

impl<P, T> Lock<P, T> {
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED),
            value: UnsafeCell::new(value),
            _phantom: PhantomData,
        }
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

    pub fn lock_async(&self) -> LockFuture<'_, P, T> {
        LockFuture::new(
            match self.state.compare_exchange_weak(
                UNLOCKED,
                LOCKED,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => LockState::Locked(LockGuard(self)),
                Err(_) => LockState::Locking(self),
            },
        )
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

    pub fn lock(&self) -> LockGuard<'_, P, T> {
        let parker = P::new();
        let waker = {
            let ptr = &parker as *const _ as *const ();
            let raw_waker = RawWaker::new(ptr, &Self::VTABLE);
            Waker::from_raw(raw_waker)
        };

        let mut future = self.lock_async();
        loop {
            let mut context = Context::from_waker(&waker);
            let pinned_future = Pin::new_unchecked(&mut future);
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
            let head = NonNull::new((state & WAITING) as *mut Waiter<P>)
                .expect("unlock without a waiter");
            let tail = {
                let mut current = head;
                loop {
                    if let Some(tail) = current.as_ref().tail.get() {
                        head.as_ref().tail.set(Some(tail));
                        break tail;
                    } else {
                        let next = current.as_ref().next.get().unwrap();
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

            let waker_notify = if be_fair { WAKER_ACQUIRED } else { WAKER_NOTIFIED };
            let waker = wait_state.waker.notify(waker_notify);
            waker.wake();
            return;
        }
    }
}

enum LockState<'a, P, T> {
    Locking(&'a Lock<P, T>),
    Waiting(&'a Lock<P, T>),
    Locked(LockGuard<'a, P, T>),
    Completed,
}

struct WaitState<P: Parker> {
    waker: AtomicWaker,
    force_fair_at: Cell<Option<P::Instant>>,
}

type Waiter<P> = Node<WaitState<P>>;

pub struct LockFuture<'a, P: Parker, T> {
    state: LockState<'a, P, T>,
    waiter: Waiter<P>,
    _pinned: PhantomPinned,
}

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
            match mut_self.state {
                LockState::Locking(lock) => match mut_self.poll_lock(ctx, lock) {
                    Poll::Ready(guard) => mut_self.state = LockState::Locked(guard),
                    Poll::Pending => mut_self.state = LockState::Waiting(lock),
                },
                LockState::Waiting(lock) => match mut_self.poll_wait(ctx, lock) {
                    Poll::Ready(Some(guard)) => mut_self.state = LockState::Locked(guard),
                    Poll::Ready(None) => mut_self.state = LockState::Locking(lock),
                    Poll::Pending => return Poll::Pending,
                },
                LockState::Locked(guard) => {
                    mut_self.state = LockState::Completed;
                    return Poll::Ready(guard);
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
                waker: AtomicWaker::new(),
                force_fair_at: Cell::new(None),
            }),
            _pinned: PhantomPinned
        }
    }

    fn poll_lock(
        &self,
        ctx: &mut Context<'_>,
        lock: &'a Lock<P, T>,
    ) -> Poll<LockGuard<'a, P, T>> {
        let mut spin = 0;
        let waiter = &self.waiter;
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
                    let wait_state = &*waiter.get();
                    wait_state.waker.prepare_poll(ctx);
                    if (&*wait_state.force_fair_at.as_ptr()).is_none() {
                        wait_state.force_fair_at.set(Some({
                            P::now() + Duration::new(0, {
                                let ptr = head.unwrap_or(NonNull::from(waiter)).as_ptr() as usize;
                                let ptr = ((ptr * 13) ^ (ptr >> 15)) & (!0u32 as usize);
                                let rng_u32: u32 = ptr.try_into().unwrap();
                                rng_u32 % 1_000_000
                            })
                        }));
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

    fn poll_wait(
        &self,
        ctx: &mut Context<'_>,
        lock: &'a Lock<P, T>,
    ) -> Poll<Option<LockGuard<'a, P, T>>> {
        let wait_state = unsafe { &*self.waiter.get() };
        match wait_state.waker.poll(ctx) {
            Poll::Ready(WAKER_ACQUIRED) => Poll::Ready(Some(LockGuard(lock))),
            Poll::Ready(WAKER_NOTIFIED) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
            _ => unreachable!("invalid waker state"),
        }
    }
}

unsafe impl<'a, P, T: Send> Send for LockFuture<'a, P, T> {}

pub struct LockGuard<'a, P, T>(&'a Lock<P, T>);

impl<'a, P: Parker, T> Drop for LockGuard<'a, P, T> {
    fn drop(&mut self) {
        unsafe { self.0.unlock() }
    }
}

impl<'a, P, T> DerefMut for LockGuard<'a, P, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.0.value.get() }
    }
}

impl<'a, P, T> Deref for LockGuard<'a, P, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.0.value.get() }
    }
}
