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
    cell::Cell,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, AtomicBool, Ordering},
};

const UNLOCKED: usize = 0;
const LOCKED: usize = 1;
const QLOCKED: usize = 2;
const WAITING: usize = !(LOCKED | QLOCKED);

#[repr(align(4))]
struct Waiter {
    next: Cell<Option<NonNull<Self>>>,
    tail: Cell<NonNull<Self>>,
    should_park: AtomicBool,
    parker: Cell<Option<thread::Thread>>,
}

pub struct Lock {
    state: AtomicUsize,
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "WTF::WordLock"
    }

    fn new() -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED),
        }
    }

    fn with<F: FnOnce()>(&self, f: F) {
        self.acquire();
        let _ = f();
        self.release();
    }
}

impl Lock {
    fn yield_now() {
        // std::thread::yield_now();
        std::sync::atomic::spin_loop_hint();
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
        let t = thread::current().id();

        loop {
            let state = self.state.load(Ordering::Relaxed);

            if state & LOCKED == 0 {
                if let Ok(_) = self.state.compare_exchange_weak(
                    state,
                    state | LOCKED,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    println!("{:?} owns lock", t);
                    return;
                }
            }

            let head = NonNull::new((state & WAITING) as *mut Waiter);
            if head.is_none() && spin < 40 {
                spin += 1;
                Self::yield_now();
                continue;
            }

            if state & QLOCKED != 0 {
                Self::yield_now();
                continue;
            }

            if let Err(_) = self.state.compare_exchange_weak(
                state,
                state | QLOCKED,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Self::yield_now();
                continue;
            }

            let waiter = Waiter {
                next: Cell::new(None),
                tail: Cell::new(NonNull::dangling()),
                should_park: AtomicBool::new(true),
                parker: Cell::new(Some(thread::current())),
            };

            waiter.tail.set(NonNull::from(&waiter));
            let new_head = if let Some(head) = head {
                let tail = head.as_ref().tail.replace(NonNull::from(&waiter));
                tail.as_ref().next.set(Some(NonNull::from(&waiter)));
                head
            } else {
                NonNull::from(&waiter)
            };

            let new_state = (new_head.as_ptr() as usize) | LOCKED;
            self.state.store(new_state, Ordering::Release);
            
            
            println!("{:?} is suspended {:b}", t, self.state.load(Ordering::Relaxed));
            while waiter.should_park.load(Ordering::Relaxed) {
                thread::park();
            }
            println!("{:?} is resumed {:b}", t, self.state.load(Ordering::Relaxed));
        }
    }

    #[inline]
    fn release(&self) {
        if self
            .state
            .compare_exchange_weak(LOCKED, UNLOCKED, Ordering::Release, Ordering::Relaxed)
            .is_err()
        {
            unsafe { self.release_slow() };
        }
    }

    #[cold]
    unsafe fn release_slow(&self) {
        let t = thread::current().id();
        loop {
            let state = self.state.load(Ordering::Relaxed);

            if state == LOCKED {
                if let Ok(_) = self.state.compare_exchange_weak(
                    LOCKED,
                    UNLOCKED,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    return;
                }
                Self::yield_now();
                continue;
            }

            if state & QLOCKED != 0 {
                Self::yield_now();
                continue;
            }

            if let Err(_) = self.state.compare_exchange_weak(
                state,
                state | QLOCKED,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Self::yield_now();
                continue;
            }

            let head = NonNull::new((state & WAITING) as *mut Waiter).unwrap();
            let next = head.as_ref().next.get();
            if let Some(next) = next {
                next.as_ref().tail.set(head.as_ref().tail.get());
            }
            
            let t = head.as_ref().parker.replace(None).unwrap();
            let new_state = next.map(|p| p.as_ptr() as usize).unwrap_or(UNLOCKED);
            self.state.store(new_state, Ordering::Release);

            head.as_ref().should_park.store(false, Ordering::Relaxed);
            t.unpark();
            return;
        }
    }
}
