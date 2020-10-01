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
    parker::Parker,
    shared::{Node, WakerParker},
};
use core::{
    pin::Pin,
    marker::PhantomData,
    cell::UnsafeCell,
    future::Future,
    ptr::NonNull,
    task::{Poll, Task, Context},
    sync::atomic::{AtomicUsize, Ordering},
};

const UNLOCKED: usize = 0;
const LOCKED: usize = 1;
const WAKING: usize = 2;
const WAITING: usize = !(LOCKED | WAKING);

#[repr(align(4))]
struct Waiter(Node<WakerParker>);

pub struct Lock {
    state: AtomicUsize,
}

impl Lock {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED),
        }
    }

    #[inline]
    pub unsafe fn is_locked(&self) -> bool {
        (self.state.load(Ordering::Relaxed) & LOCKED) != 0
    }

    #[inline]
    pub unsafe fn try_lock(&self) -> bool {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & LOCKED != 0 {
                return false;
            }
            match self.state.compare_exchange_weak(
                state,
                state | LOCKED,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(e) => state = e,
            }
        }
    }

    #[inline]
    fn lock_fast(&self) -> bool {
        self.state
            .compare_exchange_weak(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    pub unsafe fn lock_async<P: Parker>(&self) -> LockFuture<'_, P> {
        LockFuture::new(if self.lock_fast() {
            LockFutureState::Acquired
        } else {
            LockFutureState::Locking(self)
        })
    }

    #[inline]
    pub unsafe fn lock<P: Parker>(&self) {
        if !self.lock_fast() {
            self.lock_slow::<P>();
        }
    }

    #[cold]
    fn lock_slow<P: Parker>(&self) {
        let parker = P::new();
        let waker = unsafe { WakerParker::as_waker(&parker) };

        let mut ctx = Context::from_waker(&waker);
        let mut future = LockFuture::<'_, P>::new(LockFutureState::Locking(self));

        loop {
            let pinned = unsafe { Pin::new_unchecked(&mut future) };
            match pinned.poll(&mut ctx) {
                Poll::Ready(()) => return,
                Poll::Pending => parker.park(),
            }
        }
    }

    #[inline]
    pub unsafe fn unlock(&self) {
        let state = self.state.fetch_sub(LOCKED, Ordering::Release);

        if (state & WAKING == 0) && (state & WAITING != 0) {
            self.unlock_slow();
        }
    }

    #[cold]
    fn unlock_slow(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if (state & (WAKING | LOCKED) != 0) || (state & WAITING == 0) {
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
            let head = (state & WAITING) as *mut Waiter;
            let head = NonNull::new(head).expect("Lock::unlock() without a waiter");
            let tail = {
                let mut current = head;
                loop {
                    if let Some(tail) = current.as_ref().0.tail.get() {
                        head.as_ref().0.tail.set(Some(tail));
                        break tail;
                    } else {
                        let next = current.as_ref().0.next.get().expect("Lock::unlock() with invalid node links");
                        next.as_ref().0.prev.set(Some(current));
                        current = next;
                    }
                }
            };

            if state & LOCKED != 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state & !WAKING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(e) => state = e,
                }
                continue;
            }

            if let Some(new_tail) = tail.as_ref().0.prev.get() {
                head.as_ref().0.tail.set(Some(new_tail));
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
            
            (&*tail.as_ref().0.get()).unpark();
            return;
        }
    }
}

enum LockFutureState<'a> {
    Locking(&'a Lock),
    Waiting(&'a Lock),
    Acquired,
}

pub struct LockFuture<'a, P> {
    state: LockFutureState<'a>,
    waiter: UnsafeCell<Waiter>,
    _phantom: PhantomData<P>,
}

unsafe impl<'a, P> Send for LockFuture<'a, P> {}

impl<'a, P: Parker> Drop for LockFuture<'a, P> {
    fn drop(&mut self) {
        if let LockFutureState::Waiting(_) = self.state {
            self.cancel();
        }
    }
}

impl<'a, P: Parker> Future for LockFuture<'a, P> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut_self = unsafe { Pin::get_unchecked_mut(self) };

        loop {
            match mut_self.state {
                LockFutureState::Locking(lock) => match mut_self.poll_lock(ctx, lock) {
                    Poll::Ready(()) => break,
                    Poll::Pending => {
                        mut_self.state = LockFutureState::Waiting(lock);
                        return Poll::Pending;
                    }
                },
                LockFutureState::Waiting(lock) => match mut_self.poll_wait(ctx) {
                    Poll::Ready(()) => mut_self.state = LockFutureState::Locking(lock),
                    Poll::Pending => return Poll::Pending,
                },
                LockFutureState::Acquired => {
                    unreachable!("LockFuture polled after completion");
                }
            }
        }

        mut_self.state = LockFutureState::Acquired;
        return Poll::Ready(());
    }
}

impl<'a, P: Parker> LockFuture<'a, P> {
    fn new(future_state: LockFutureState<'a>) -> Self {
        Self {
            state: future_state,
            waiter: UnsafeCell::new(Waiter::new(WakerParker::new())),
            _phantom: PhantomData,
        }
    }

    fn poll_lock(&self, ctx: &Context<'_>, lock: &'a Lock) -> Poll<()> {
        let waiter = unsafe { &*self.waiter.get() };
        let mut spin_iter = 0;
        let mut state = lock.state.load(Ordering::Relaxed);

        loop {
            let new_state;
            let head = NonNull::new((state & WAITING) as *mut Waiter);
            let head = head.map(|p| p.cast::<Node<WakerParker>>());

            if state & LOCKED == 0 {
                new_state = state | LOCKED;
            } else if head.is_none() && P::yield_now(spin_iter) {
                spin_iter = spin_iter.wrapping_add(1);
                state = lock.state.load(Ordering::Relaxed);
                continue;
            } else {
                new_state = (state & !WAITING) | (&waiter as *const _ as usize);
                waiter.0.next.set(head);
                waiter.0.tail.set(match head {
                    Some(_) => None,
                    None => Some(NonNull::from(&waiter.0)),
                });

                unsafe {
                    let waker_parker = &*waiter.0.get();
                    waker_parker.prepare(ctx);
                }
            }

            match self.state.compare_exchange_weak(
                state,
                new_state,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) if state & LOCKED == 0 => return Poll::Ready(()),
                Ok(_) => return Poll::Pending,
                Err(e) => state = e,
            }
        }
    }

    fn poll_wait(&self, ctx: &Context<'_>) -> Poll<()> {
        unsafe {
            let waiter = &*self.waiter.get();
            let waker_parker = &*waiter.0.get();

            match waker_parker.park(ctx) {
                Ok(0) => Poll::Ready(()),
                Ok(t) => unreachable!("invalid wake token {:?}", t),
                Err(_) => Poll::Pending,
            }
        }
    }

    #[cold]
    fn cancel(&self) {
        unsafe {
            let waiter = &*self.waiter.get();
            let waker_parker = &*waiter.0.get();

            match waker_parker.park(ctx) {
                Ok(0) => Poll::Ready(()),
                Ok(t) => unreachable!("invalid wake token {:?}", t),
                Err(_) => Poll::Pending,
            }
        }
    }
}
