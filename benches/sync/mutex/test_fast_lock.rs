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
    cell::{Cell, UnsafeCell},
    hint::unreachable_unchecked,
    mem::MaybeUninit,
    ptr::{drop_in_place, write, NonNull},
    sync::atomic::{spin_loop_hint, AtomicU8, AtomicUsize, Ordering},
    thread,
};

pub struct Lock {
    state: AtomicUsize,
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "test_fast_lock"
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
const WAITING: usize = !((1 << 9) - 1);

#[repr(align(512))]
struct Waiter {
    prev: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    next: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    tail: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    event: UnsafeCell<MaybeUninit<Event>>,
}

const EVENT_WAITING: usize = 0;
const EVENT_NOTIFIED: usize = 1;
const EVENT_HANDOFF: usize = 2;

struct Event {
    thread: thread::Thread,
    notified: AtomicUsize,
}

impl Lock {
    fn byte_state(&self) -> &AtomicU8 {
        unsafe { &*(&self.state as *const _ as *const _) }
    }

    #[inline]
    fn acquire(&self) {
        if self.byte_state().swap(LOCKED, Ordering::Acquire) != UNLOCKED {
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

            if state & (LOCKED as usize) == 0 {
                if self.byte_state().swap(LOCKED, Ordering::Acquire) == UNLOCKED {
                    break;
                } else {
                    spin_loop_hint();
                    state = self.state.load(Ordering::Relaxed);
                    continue;
                }
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
                    write(
                        event_ptr,
                        MaybeUninit::new(Event {
                            thread: thread::current(),
                            notified: AtomicUsize::new(EVENT_WAITING),
                        }),
                    );
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

            let event = &*(&*event_ptr).as_ptr();
            let unset_waking = loop {
                match event.notified.load(Ordering::Acquire) {
                    EVENT_WAITING => thread::park(),
                    EVENT_NOTIFIED => break true,
                    EVENT_HANDOFF => break false,
                    _ => unreachable_unchecked(),
                }
            };

            event.notified.store(EVENT_WAITING, Ordering::Relaxed);
            if unset_waking {
                self.state.fetch_and(!WAKING, Ordering::Relaxed);
            }

            spin = 0;
            waiter.prev.set(MaybeUninit::new(None));
            state = self.state.load(Ordering::Relaxed);
        }

        if has_event {
            drop_in_place((&mut *event_ptr).as_mut_ptr());
        }
    }

    #[inline]
    fn release(&self) {
        self.byte_state().store(UNLOCKED, Ordering::Release);

        let state = self.state.load(Ordering::Relaxed);
        if state != (UNLOCKED as usize) {
            unsafe { self.release_slow() };
        }
    }

    #[cold]
    unsafe fn release_slow(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if (state & WAITING == 0) || (state & ((LOCKED as usize) | WAKING) != 0) {
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
            let head = NonNull::new_unchecked((state & WAITING) as *mut Waiter);
            let tail = head.as_ref().tail.get().assume_init().unwrap_or_else(|| {
                let mut current = head;
                loop {
                    let next = current.as_ref().next.get().assume_init();
                    let next = next.unwrap_or_else(|| unreachable_unchecked());
                    next.as_ref().prev.set(MaybeUninit::new(Some(current)));
                    current = next;
                    if let Some(tail) = current.as_ref().tail.get().assume_init() {
                        head.as_ref().tail.set(MaybeUninit::new(Some(tail)));
                        break tail;
                    }
                }
            });

            if state & (LOCKED as usize) != 0 {
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

            let notify = EVENT_HANDOFF;
            if let Some(new_tail) = tail.as_ref().prev.get().assume_init() {
                head.as_ref().tail.set(MaybeUninit::new(Some(new_tail)));
                // notify = EVENT_NOTIFIED;
                // std::sync::atomic::fence(Ordering::Release);
                self.state.fetch_and(!WAKING, Ordering::Release);
            } else if let Err(e) = self.state.compare_exchange_weak(
                state,
                UNLOCKED as usize,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                state = e;
                continue;
            }

            let event = &*(&*tail.as_ref().event.get()).as_ptr();
            event.notified.store(notify, Ordering::Release);
            event.thread.unpark();
            return;
        }
    }
}
