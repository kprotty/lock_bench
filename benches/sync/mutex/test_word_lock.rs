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
    sync::atomic::{spin_loop_hint, AtomicUsize, Ordering},
    thread,
};

pub struct Lock {
    state: AtomicUsize,
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "test_word_lock"
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

const UNLOCKED: usize = 0;
const LOCKED: usize = 1;
const WAKING: usize = 2;
const WAITING: usize = !(LOCKED | WAKING);

#[repr(align(4))]
struct Waiter {
    state: AtomicUsize,
    prev: Cell<Option<NonNull<Self>>>,
    next: Cell<Option<NonNull<Self>>>,
    tail: Cell<Option<NonNull<Self>>>,
    thread: Cell<Option<thread::Thread>>,
}

const EVENT_WAITING: usize = 0;
const EVENT_NOTIFIED: usize = 1;

impl Lock {
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
        let max_spin = if cfg!(windows) { 10 } else { 5 };

        let waiter = Waiter {
            state: AtomicUsize::new(EVENT_WAITING),
            prev: Cell::new(None),
            next: Cell::new(None),
            tail: Cell::new(None),
            thread: Cell::new(None),
        };

        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            let new_state;
            let head = NonNull::new((state & WAITING) as *mut Waiter);

            if state & LOCKED == 0 {
                new_state = state | LOCKED;
            } else if head.is_none() && spin < max_spin {
                spin += 1;
                (0..(1 << spin)).for_each(|_| spin_loop_hint());
                state = self.state.load(Ordering::Relaxed);
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
                Ordering::Relaxed,
            ) {
                state = e;
                continue;
            }

            if state & LOCKED == 0 {
                return;
            }

            loop {
                match waiter.state.load(Ordering::Acquire) {
                    EVENT_WAITING => thread::park(),
                    EVENT_NOTIFIED => break,
                    _ => unreachable!(),
                }
            }

            spin = 0;
            state = self.state.load(Ordering::Relaxed);
        }
    }

    #[inline]
    fn release(&self) {
        let state = self.state.fetch_sub(LOCKED, Ordering::Release);
        if (state & WAKING == 0) && (state & WAITING != 0) {
            unsafe { self.release_slow() };
        }
    }

    #[cold]
    unsafe fn release_slow(&self) {
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
            let head = NonNull::new((state & WAITING) as *mut Waiter).unwrap();
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

            if let Some(new_tail) = tail.as_ref().prev.get() {
                head.as_ref().tail.set(Some(new_tail));
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

            let thread = tail.as_ref().thread.replace(None).unwrap();
            tail.as_ref().state.store(EVENT_NOTIFIED, Ordering::Release);
            thread.unpark();
            return;
        }
    }
}
