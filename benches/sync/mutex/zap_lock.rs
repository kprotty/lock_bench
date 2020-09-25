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
    cell::Cell,
    ptr::NonNull,
    sync::atomic::{spin_loop_hint, AtomicUsize, AtomicU8, Ordering},
    thread,
};

pub struct Lock {
    state: AtomicUsize,
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "zap_lock"
    }

    fn new() -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED as usize),
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
const WAKING: usize = 1 << 8;
const WAITING: usize = !((1usize << 9) - 1);

#[repr(align(512))]
struct Waiter {
    state: AtomicUsize,
    prev: Cell<Option<NonNull<Self>>>,
    next: Cell<Option<NonNull<Self>>>,
    tail: Cell<Option<NonNull<Self>>>,
    thread: Cell<Option<thread::Thread>>,
}

impl Waiter {
    unsafe fn find_tail(&self) -> NonNull<Self> {
        let mut current = NonNull::from(self);
        loop {
            if let Some(tail) = current.as_ref().tail.get() {
                self.tail.set(Some(tail));
                break tail;
            } else {
                let next = current.as_ref().next.get().unwrap();
                next.as_ref().prev.set(Some(current));
                current = next;
            }
        }
    }
}

const EVENT_WAITING: usize = 0;
const EVENT_NOTIFIED: usize = 1;

impl Lock {
    #[inline]
    fn byte_state(&self) -> &AtomicU8 {
        unsafe { &*(&self.state as *const _ as *const _) }
    }

    #[inline]
    fn acquire(&self) {
        if self
            .byte_state()
            .swap(LOCKED, Ordering::Acquire)
            != UNLOCKED
        {
            unsafe { self.acquire_slow() };
        }
    }

    #[cold]
    unsafe fn acquire_slow(&self) {
        let mut spin = 0;
        let mut is_waking = false;
        let waiter = Waiter {
            state: AtomicUsize::new(EVENT_WAITING),
            prev: Cell::new(None),
            next: Cell::new(None),
            tail: Cell::new(None),
            thread: Cell::new(None),
        };

        let mut state = self.state.load(Ordering::Acquire);
        loop {
            let mut new_state;
            let head = NonNull::new((state & WAITING) as *mut Waiter);

            if state & (LOCKED as usize) == 0 {
                new_state = state | (LOCKED as usize);
                if is_waking {
                    new_state &= !WAKING;
                    if let Some(head) = head {
                        if head == NonNull::from(&waiter) {
                            new_state &= !WAITING;
                        } else {
                            let _ = head.as_ref().find_tail();
                            head.as_ref().tail.set(waiter.prev.get());
                        }
                    }
                }

            } else if is_waking {
                new_state = state & !WAKING;
                if let Some(head) = head {
                    let _ = head.as_ref().find_tail();
                    head.as_ref().tail.set(Some(NonNull::from(&waiter)));
                }

                if (&*waiter.thread.as_ptr()).is_none() {
                    waiter.thread.set(Some(thread::current()));
                    waiter.state.store(EVENT_WAITING, Ordering::Relaxed);
                }
            } else if head.is_none() && spin <= 10 {
                if spin <= 3 {
                    (0..(1 << spin)).for_each(|_| spin_loop_hint());
                } else if cfg!(windows) {
                    thread::sleep(std::time::Duration::new(0, 0));
                } else {
                    thread::yield_now();
                }
                spin += 1;
                state = self.state.load(Ordering::Acquire);
                continue;
            } else {
                new_state = (state & !WAITING) | (&waiter as *const _ as usize);
                waiter.next.set(head);
                waiter.tail.set(match head {
                    Some(_) => None,
                    None => Some(NonNull::from(&waiter)),
                });

                if (&*waiter.thread.as_ptr()).is_none() {
                    waiter.prev.set(None);
                    waiter.thread.set(Some(thread::current()));
                    waiter.state.store(EVENT_WAITING, Ordering::Relaxed);
                }
            }

            if let Err(e) = self.state.compare_exchange_weak(
                state,
                new_state,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                state = e;
                continue;
            }

            if state & (LOCKED as usize) == 0 {
                return;
            }

            loop {
                match waiter.state.load(Ordering::Acquire) {
                    EVENT_WAITING => thread::park(),
                    EVENT_NOTIFIED => break,
                    _ => unreachable!(),
                }
            }

            // spin = 0;
            is_waking = true;
            state = self.state.load(Ordering::Relaxed);
        }
    }

    #[inline]
    fn release(&self) {
        self.byte_state().store(UNLOCKED, Ordering::Release);

        let state = self.state.load(Ordering::Acquire);
        if (state & (WAKING | (LOCKED as usize)) == 0) && (state & WAITING != 0) {
            unsafe { self.release_slow(state) };
        }
    }

    #[cold]
    unsafe fn release_slow(&self, mut state: usize) {
        loop {
            if (state & (WAKING | (LOCKED as usize)) != 0) || (state & WAITING == 0) {
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

        let head = NonNull::new((state & WAITING) as *mut Waiter).unwrap();
        let tail = head.as_ref().find_tail();

        let thread = tail.as_ref().thread.replace(None).unwrap();
        tail.as_ref().state.store(EVENT_NOTIFIED, Ordering::Release);
        thread.unpark();
    }
}
