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
    ptr::NonNull,
    cell::Cell,
    sync::{Mutex, atomic::{spin_loop_hint, AtomicI32, Ordering}}
};

pub struct Lock {
    state: AtomicI32,
    waiters: Mutex<Option<NonNull<Waiter>>>,
}

unsafe impl Send for Lock {}
unsafe impl Sync for Lock {}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "go_lock"
    }

    fn new() -> Self {
        Self {
            state: AtomicI32::new(UNLOCKED),
            waiters: Mutex::new(None),
        }
    }

    fn with<F: FnOnce()>(&self, f: F) {
        self.acquire();
        let _ = f();
        self.release();
    }
}

const UNLOCKED: i32 = 0;
const LOCKED: i32 = 1;
const SLEEPING: i32 = 2;

struct Waiter {
    next: Cell<Option<NonNull<Self>>>,
    tail: Cell<NonNull<Self>>,
    notified: AtomicI32,
    thread: Cell<Option<thread::Thread>>,
}

impl Lock {
    #[inline]
    fn acquire(&self) {
        let state = self.state.swap(LOCKED, Ordering::Acquire);
        if state != UNLOCKED {
            self.acquire_slow(state);
        }
    }

    #[cold]
    fn acquire_slow(&self, mut state: i32) {
        let mut spin = 0;
        let mut wait = state;
        state = self.state.load(Ordering::Relaxed);
        
        loop {
            if state == UNLOCKED {
                if let Ok(_) = self.state.compare_exchange_weak(
                    UNLOCKED,
                    wait,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    break;
                }
            }

            if spin < 4 {
                (0..32).for_each(|_| spin_loop_hint());
                state = self.state.load(Ordering::Relaxed);
                spin += 1;

            } else if spin < 5 {
                if cfg!(windows) {
                    thread::sleep(std::time::Duration::new(0, 0));
                } else {
                    thread::yield_now();
                }
                state = self.state.load(Ordering::Relaxed);
                spin += 1;

            } else {
                state = self.state.swap(SLEEPING, Ordering::Acquire);
                if state == UNLOCKED {
                    break;
                }
                
                spin = 0;
                wait = SLEEPING;
                while state == SLEEPING {
                    unsafe { self.futex_wait(SLEEPING) };
                    state = self.state.load(Ordering::Relaxed);
                }
            }
        }
    }

    #[inline]
    fn release(&self) {
        if self.state.swap(UNLOCKED, Ordering::Release) == SLEEPING {
            self.release_slow();
        }
    }

    #[cold]
    fn release_slow(&self) {
        unsafe { self.futex_wake(1) };
    }

    unsafe fn futex_wait(&self, value: i32) {
        let mut queue = self.waiters.lock().unwrap();
        if self.state.load(Ordering::Acquire) != value {
            return;
        }

        let waiter = Waiter {
            next: Cell::new(None),
            tail: Cell::new(NonNull::dangling()),
            thread: Cell::new(Some(thread::current())),
            notified: AtomicI32::new(0),
        };

        waiter.tail.set(NonNull::from(&waiter));
        if let Some(head) = *queue {
            let tail = head.as_ref().tail.get();
            tail.as_ref().next.set(Some(NonNull::from(&waiter)));
            head.as_ref().tail.set(NonNull::from(&waiter));
        } else {
            *queue = Some(NonNull::from(&waiter));
        }

        std::mem::drop(queue);
        while waiter.notified.load(Ordering::Acquire) == 0 {
            thread::park();
        }
    }

    unsafe fn futex_wake(&self, count: u32) {
        let mut queue = self.waiters.lock().unwrap();
        let mut waiters: Option<NonNull<Waiter>> = None;

        for _ in 0..count {
            let waiter = match *queue {
                Some(waiter) => waiter,
                None => break,
            };

            *queue = waiter.as_ref().next.get();
            if let Some(next) = *queue {
                next.as_ref().tail.set(waiter.as_ref().tail.get());
            }

            waiter.as_ref().next.set(None);
            waiter.as_ref().tail.set(waiter);
            if let Some(head) = waiters {
                let tail = head.as_ref().tail.get();
                tail.as_ref().next.set(Some(waiter));
                head.as_ref().tail.set(waiter);
            } else {
                waiters = Some(waiter);
            }
        }

        std::mem::drop(queue);

        while let Some(waiter) = waiters {
            waiters = waiter.as_ref().next.get();
            let thread = waiter.as_ref().thread.replace(None).unwrap();
            waiter.as_ref().notified.store(1, Ordering::Release);
            thread.unpark();
        }
    }
}
