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
    cell::{Cell, UnsafeCell},
    ptr::{write, drop_in_place, NonNull},
    mem::MaybeUninit,
    hint::unreachable_unchecked,
    sync::atomic::{spin_loop_hint, AtomicUsize, Ordering},
};

pub struct Lock {
    state: AtomicUsize
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "test_lock"
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
const WAITING: usize = !LOCKED;

#[repr(align(2))]
struct Waiter {
    prev: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    next: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    tail: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    event: UnsafeCell<MaybeUninit<(thread::Thread, AtomicUsize)>>,
}

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
        let mut has_event = false;
        let waiter = Waiter {
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
            event: UnsafeCell::new(MaybeUninit::uninit()),
        };

        let event_ptr = waiter.event.get();
        let mut state = self.state.load(Ordering::Relaxed);

        loop {
            let new_state;
            let head = NonNull::new((state & WAITING) as *mut Waiter);

            if state & LOCKED == 0 {
                new_state = state | LOCKED;
            } else if head.is_none() && (spin <= 5) {
                (0..(1 << spin)).for_each(|_| spin_loop_hint());
                spin += 1;
                state = self.state.load(Ordering::Relaxed);
                continue;
            } else {
                new_state = (state & !WAITING) | (&waiter as *const _ as usize);
                waiter.next.set(MaybeUninit::new(head));
                waiter.tail.set(MaybeUninit::new(match head {
                    Some(_) => None,
                    None => Some(NonNull::from(&waiter)),
                }));
                if !has_event {
                    has_event = true;
                    waiter.prev.set(MaybeUninit::new(None));
                    write(event_ptr, MaybeUninit::new((thread::current(), AtomicUsize::new(0))));
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
                if has_event {
                    drop_in_place((&mut *event_ptr).as_mut_ptr());
                }
                return;
            }
            
            let event = &*(&*event_ptr).as_ptr();
            while event.1.load(Ordering::Acquire) == 0 {
                thread::park();
            }

            event.1.store(0, Ordering::Relaxed);
            waiter.prev.set(MaybeUninit::new(None));
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
        let mut state = self.state.load(Ordering::Acquire);

        loop {
            let head = &*((state & WAITING) as *mut Waiter);
            let tail = &*head
                .tail
                .get()
                .assume_init()
                .unwrap_or_else(|| {
                    let mut current = NonNull::from(head);
                    loop {
                        let next = current.as_ref().next.get().assume_init();
                        let next = next.unwrap_or_else(|| unreachable_unchecked());
                        next.as_ref().prev.set(MaybeUninit::new(Some(current)));
                        current = next;
                        if let Some(tail) = current.as_ref().tail.get().assume_init() {
                            head.tail.set(MaybeUninit::new(Some(tail)));
                            break tail;
                        }
                    }
                })
                .as_ptr();

            let new_tail = tail.prev.get().assume_init();

            let mut new_state = state & !LOCKED;
            if let Some(new_tail) = new_tail {
                head.tail.set(MaybeUninit::new(Some(new_tail)));
            } else {
                new_state &= !WAITING;
            }

            if let Err(e) = self.state.compare_exchange_weak(
                state,
                new_state,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                if new_tail.is_some() {
                    head.tail.set(MaybeUninit::new(Some(NonNull::from(tail))));
                }
                state = e;
                continue;
            }

            let event = &*(&*tail.event.get()).as_ptr();
            event.1.store(1, Ordering::Release);
            event.0.unpark();
            return;
        }
    }
}
