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

use std::{
    thread,
    time::{Instant, Duration},
    convert::TryInto,
    cell::{Cell, UnsafeCell},
    ptr::{drop_in_place, NonNull},
    mem::MaybeUninit,
    hint::unreachable_unchecked,
    sync::atomic::{spin_loop_hint, AtomicU8, Ordering},
};

pub struct Lock {
    state: AtomicU8,
    prng: Cell<u16>,
    lock: super::test_fast_lock::Lock,
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
            state: AtomicU8::new(UNLOCKED),
            prng: Cell::new(0),
            lock: super::test_fast_lock::Lock::new(),
            queue: UnsafeCell::new(None),
        }
    }

    fn with<F: FnOnce()>(&self, f: F) {
        self.acquire();
        let _ = f();
        self.release();
    }
}

const UNLOCKED: u8 = 0;
const LOCKED: u8 = 1;
const PARKED: u8 = 2;

const EVENT_WAITING: u8 = 0;
const EVENT_NOTIFIED: u8 = 1;
const EVENT_HANDOFF: u8 = 2;

struct Waiter {
    next: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    tail: Cell<MaybeUninit<NonNull<Self>>>,
    event: UnsafeCell<MaybeUninit<Event>>,
}

struct Event {
    thread: thread::Thread,
    notified: AtomicU8,
    force_fair_at: Instant,
}

impl Lock {
    unsafe fn with_queue<F>(&self, f: impl FnOnce(&mut Option<NonNull<Waiter>>) -> F) -> F {
        use super::Lock;
        let mut ret = None;
        self.lock.with(|| ret = Some(f(&mut *self.queue.get())));
        ret.unwrap_or_else(|| unreachable_unchecked())
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
        let mut has_event = false;
        let waiter = Waiter {
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
            event: UnsafeCell::new(MaybeUninit::uninit()),
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
                if spin <= 5 {
                    (0..(1 << spin)).for_each(|_| spin_loop_hint());
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

                waiter.next.set(MaybeUninit::new(None));
                waiter.tail.set(MaybeUninit::new(NonNull::from(&waiter)));
                if let Some(head) = *queue {
                    let tail = head.as_ref().tail.get().assume_init();
                    tail.as_ref().next.set(MaybeUninit::new(Some(NonNull::from(&waiter))));
                    head.as_ref().tail.set(MaybeUninit::new(NonNull::from(&waiter)));
                } else {
                    *queue = Some(NonNull::from(&waiter));
                }

                let thread = if has_event {
                    let event = &mut *(&mut *waiter.event.get()).as_mut_ptr();
                    drop_in_place(&mut event.force_fair_at);
                    std::ptr::read(&event.thread)
                } else {
                    has_event = true;
                    thread::current()
                };

                std::ptr::write(waiter.event.get(), MaybeUninit::new(Event {
                    thread: thread,
                    notified: AtomicU8::new(EVENT_WAITING),
                    force_fair_at: {
                        let mut xs = self.prng.get();
                        if xs == 0 {
                            let seed = (&self.state as *const _ as usize) >> 8;
                            xs = (seed & 0xffff).try_into().unwrap_or_else(|_| unreachable_unchecked());
                        }

                        let mut xorshift16 = || {
                            xs ^= xs << 7;
                            xs ^= xs >> 9;
                            xs ^= xs << 8;
                            xs
                        };
                        
                        let low = xorshift16();
                        let high = xorshift16();
                        let rng = ((high as u32) << 16) | (low as u32);
                        self.prng.set(xs);

                        let timeout_ns = rng % 1_000_000;
                        Instant::now() + Duration::new(0, timeout_ns)
                    },
                }));

                true
            }) {
                let event = &*(&*waiter.event.get()).as_ptr();
                if loop {
                    match event.notified.load(Ordering::Acquire) {
                        EVENT_WAITING => thread::park(),
                        EVENT_NOTIFIED => break false,
                        EVENT_HANDOFF => break true,
                        _ => unreachable_unchecked(),
                    }
                } {
                    break;
                }
            }

            spin = 0;
            state = self.state.load(Ordering::Relaxed);
        }

        if has_event {
            drop_in_place((&mut *waiter.event.get()).as_mut_ptr());
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
        if let Some((event, notify)) = self.with_queue(|queue| {
            (*queue).map(|head| {
                let head = &*head.as_ptr();
                let event = &*(&*head.event.get()).as_ptr();

                *queue = head.next.get().assume_init();
                let has_more = (*queue).is_some();
                if let Some(next) = *queue {
                    next.as_ref().tail.set(head.tail.get());
                }

                let notify = if Instant::now() >= event.force_fair_at {
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

                Some((event, notify))
            }).unwrap_or_else(|| {
                self.state.store(UNLOCKED, Ordering::Release);
                None
            })
        }) {
            event.notified.store(notify, Ordering::Release);
            event.thread.unpark();
        }
    }
}
