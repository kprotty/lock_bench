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

use super::instant::Instant;
use std::{
    thread,
    time::Duration,
    cell::{Cell, UnsafeCell},
    ptr::NonNull,
    sync::atomic::{spin_loop_hint, AtomicUsize, Ordering},
};

type InnerLock = super::test_word_lock::Lock;

pub struct Lock {
    state: AtomicUsize,
    lock: InnerLock,
    queue: UnsafeCell<Option<NonNull<Waiter>>>,
}

unsafe impl Send for Lock {}
unsafe impl Sync for Lock {}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "test_fair_lock"
    }

    fn new() -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED),
            lock: InnerLock::new(),
            queue: UnsafeCell::new(None),
        }
    }

    fn with<F: FnOnce()>(&self, f: F) {
        self.acquire();
        let _ = f();
        self.release();
    }
}

const UNLOCKED: usize = 0;
const LOCKED: usize = 1;
const PARKED: usize = 2;

const EVENT_WAITING: usize = 0;
const EVENT_NOTIFIED: usize = 1;
const EVENT_HANDOFF: usize = 2;

struct Waiter {
    next: Cell<Option<NonNull<Self>>>,
    tail: Cell<NonNull<Self>>,
    state: AtomicUsize,
    force_fair_at: Cell<Option<Instant>>,
    thread: Cell<Option<thread::Thread>>,
}

impl Lock {
    unsafe fn with_queue<F>(&self, f: impl FnOnce(&mut Option<NonNull<Waiter>>) -> F) -> F {
        use super::Lock;
        let mut ret = None;
        self.lock.with(|| ret = Some(f(&mut *self.queue.get())));
        ret.unwrap()
    } 

    #[inline]
    fn acquire(&self) {
        if self
            .state
            .compare_exchange_weak(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            unsafe { self.acquire_slow() };
        }
    }

    #[cold]
    unsafe fn acquire_slow(&self) {
        let mut spin = 0;
        let waiter = Waiter {
            next: Cell::new(None),
            tail: Cell::new(NonNull::dangling()),
            state: AtomicUsize::new(EVENT_WAITING),
            force_fair_at: Cell::new(None),
            thread: Cell::new(None),
        };

        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & LOCKED == 0 {
                match self.state.compare_exchange_weak(
                    state,
                    state | LOCKED,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(e) => state = e,
                }
                continue;
            }

            if state & PARKED == 0 {
                if spin <= 10 {
                    if spin <= 3 {
                        (0..(1 << spin)).for_each(|_| spin_loop_hint());
                    } else if cfg!(windows) {
                        thread::sleep(Duration::new(0, 0));
                    } else {
                        thread::yield_now();
                    }
                    spin += 1;
                    state = self.state.load(Ordering::Relaxed);
                    continue;
                } else if let Err(e) = self.state.compare_exchange_weak(
                    state,
                    state | PARKED,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    state = e;
                    continue;
                }
            }

            if self.with_queue(|queue| {
                if self.state.load(Ordering::Relaxed) != (LOCKED | PARKED) {
                    return false;
                }

                waiter.next.set(None);
                waiter.tail.set(NonNull::from(&waiter));
                waiter.state.store(EVENT_WAITING, Ordering::Relaxed);

                if let Some(head) = *queue {
                    let tail = head.as_ref().tail.get();
                    tail.as_ref().next.set(Some(NonNull::from(&waiter)));
                    head.as_ref().tail.set(NonNull::from(&waiter));
                } else {
                    *queue = Some(NonNull::from(&waiter));
                }

                if (&*waiter.thread.as_ptr()).is_none() {
                    waiter.thread.set(Some(thread::current()));
                }

                if (&*waiter.force_fair_at.as_ptr()).is_none() {
                    waiter.force_fair_at.set(Some(Instant::now() + Duration::new(0, {
                        // use std::convert::TryInto;
                        // let rng = queue.unwrap_or(NonNull::from(&waiter)).as_ptr() as usize;
                        // let rng = (13 * rng) ^ (rng >> 15);
                        // (rng % 1_000_000).try_into().unwrap()
                        1_000_000
                    })));
                }

                true
            }) {
                loop {
                    match waiter.state.load(Ordering::Acquire) {
                        EVENT_WAITING => thread::park(),
                        EVENT_NOTIFIED => break,
                        EVENT_HANDOFF => return,
                        _ => unreachable!(),
                    }
                }
            }

            spin = 0;
            state = self.state.load(Ordering::Relaxed);
        }
    }

    #[inline]
    fn release(&self) {
        if self
            .state
            .compare_exchange(LOCKED, UNLOCKED, Ordering::Release, Ordering::Relaxed)
            .is_err()
        {
            unsafe { self.release_slow() };
        }
    }

    #[cold]
    unsafe fn release_slow(&self) {
        if let Some((waiter, notify)) = self.with_queue(|queue| {
            (*queue).map(|head| {
                let waiter = &*head.as_ptr();
                *queue = waiter.next.get();

                let has_more = (*queue).is_some();
                if let Some(next) = *queue {
                    next.as_ref().tail.set(waiter.tail.get());
                }

                let force_fair_at = waiter.force_fair_at.get().unwrap();
                let notify = if Instant::now() >= force_fair_at {
                    if !has_more {
                        self.state.store(LOCKED, Ordering::Relaxed);
                    }
                    EVENT_HANDOFF
                } else if has_more {
                    self.state.store(PARKED, Ordering::Release);
                    EVENT_NOTIFIED
                } else {
                    self.state.store(UNLOCKED, Ordering::Release);
                    EVENT_NOTIFIED
                };

                Some((waiter, notify))
            }).unwrap_or_else(|| {
                self.state.store(UNLOCKED, Ordering::Release);
                None
            })
        }) {
            let thread = waiter.thread.replace(None).unwrap();
            waiter.state.store(notify, Ordering::Release);
            thread.unpark();
        }
    }
}
