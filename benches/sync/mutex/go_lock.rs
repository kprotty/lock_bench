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
    sync::atomic::{spin_loop_hint, AtomicI32, Ordering},
};

use self::futex::Futex;

pub struct Lock {
    state: AtomicI32,
    futex: Futex,
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "go_lock"
    }

    fn new() -> Self {
        Self {
            state: AtomicI32::new(UNLOCKED),
            futex: Futex::new(),
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
                    unsafe { self.futex.wait(&self.state, SLEEPING) };
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
        unsafe { self.futex.wake(&self.state) };
    }
}

#[cfg(windows)]
mod futex {
    use std::{
        mem::transmute,
        sync::atomic::{AtomicUsize, AtomicI32, Ordering},
    };

    pub struct Futex {}

    static WAIT: AtomicUsize = AtomicUsize::new(0);
    static WAKE: AtomicUsize = AtomicUsize::new(0);

    type WakeByAddressSingle = extern "system" fn(Address: usize);
    type WaitOnAddress = extern "system" fn(
        Address: usize,
        CompareAddress: usize,
        AddressSize: usize,
        dwMilliseconds: u32,
    ) -> bool;

    impl Futex {
        pub fn new() -> Self {
            Self{}
        }

        pub unsafe fn wait(&self, state: &AtomicI32, value: i32) {
            let mut wait_fn = WAIT.load(Ordering::Relaxed);
            if wait_fn == 0 {
                wait_fn = Self::get_slow(b"WaitOnAddress\0".as_ptr());
                WAIT.store(wait_fn, Ordering::Relaxed);
            }

            let wait_fn: WaitOnAddress = transmute(wait_fn);
            while state.load(Ordering::Acquire) == value {
                let _ = (wait_fn)(
                    state as *const _ as usize,
                    &value as *const _ as usize,
                    std::mem::size_of::<AtomicI32>(),
                    !0u32
                );
            }
        }

        pub unsafe fn wake(&self, state: &AtomicI32) {
            let mut wake_fn = WAKE.load(Ordering::Relaxed);
            if wake_fn == 0 {
                wake_fn = Self::get_slow(b"WakeByAddressSingle\0".as_ptr());
                WAKE.store(wake_fn, Ordering::Relaxed);
            }

            let wake_fn: WakeByAddressSingle = transmute(wake_fn);
            (wake_fn)(state as *const _ as usize);
        }

        #[cold]
        unsafe fn get_slow(func: *const u8) -> usize {
            #[link(name = "kernel32")]
            extern "system" {
                fn GetModuleHandleA(path: *const u8) -> usize;
                fn GetProcAddress(dll: usize, func: *const u8) -> usize;
            }

            let dll = GetModuleHandleA(b"api-ms-win-core-synch-l1-2-0.dll\0".as_ptr());
            assert_ne!(dll, 0);
            
            let addr = GetProcAddress(dll, func);
            assert_ne!(addr, 0);

            addr
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod futex {
    use super::{AtomicI32, Ordering, super as mutex};
    use std::{
        thread,
        ptr::NonNull,
        cell::{Cell, UnsafeCell},
    };

    type InnerLock = mutex::spin_lock::Lock;

    struct Waiter {
        next: Cell<Option<NonNull<Self>>>,
        tail: Cell<NonNull<Self>>,
        notified: AtomicI32,
        thread: Cell<Option<thread::Thread>>,
    }

    pub struct Futex {
        lock: InnerLock,
        waiters: UnsafeCell<Option<NonNull<Waiter>>>,
    }

    unsafe impl Send for Futex {}
    unsafe impl Sync for Futex {}

    impl Futex {
        pub fn new() -> Self {
            Self {
                lock: InnerLock::new(),
                waiters: UnsafeCell::new(None),
            }
        }

        unsafe fn with_queue<F>(&self, f: impl FnOnce(&mut Option<NonNull<Waiter>>) -> F) -> F {
            use mutex::Lock;
            let mut res = None;
            self.lock.with(|| res = Some(f(&mut *self.waiters.get())));
            res.unwrap()
        }

        pub unsafe fn wait(&self, state: &AtomicI32, value: i32) {
            let mut stack_waiter = None;
            if self.with_queue(|queue| {
                if state.load(Ordering::Acquire) != value {
                    return false;
                }

                stack_waiter = Some(Waiter {
                    next: Cell::new(None),
                    tail: Cell::new(NonNull::dangling()),
                    thread: Cell::new(Some(thread::current())),
                    notified: AtomicI32::new(0),
                });

                let waiter = stack_waiter.as_ref().unwrap();
                waiter.tail.set(NonNull::from(waiter));
                if let Some(head) = *queue {
                    let tail = head.as_ref().tail.get();
                    tail.as_ref().next.set(Some(NonNull::from(waiter)));
                    head.as_ref().tail.set(NonNull::from(waiter));
                } else {
                    *queue = Some(NonNull::from(waiter));
                }

                true
            }) {
                let waiter = stack_waiter.as_ref().unwrap();
                while waiter.notified.load(Ordering::Acquire) == 0 {
                    thread::park();
                }
            }
        }

        pub unsafe fn wake(&self, state: &AtomicI32) {
            if let Some(waiter) = self.with_queue(|queue| {
                let waiter = match *queue {
                    Some(waiter) => waiter,
                    None => return None,
                };
    
                *queue = waiter.as_ref().next.get();
                if let Some(next) = *queue {
                    next.as_ref().tail.set(waiter.as_ref().tail.get());
                }
    
                Some(waiter)
            }) {
                let thread = waiter.as_ref().thread.replace(None).unwrap();
                waiter.as_ref().notified.store(1, Ordering::Release);
                thread.unpark();
            }
        }
    }
}