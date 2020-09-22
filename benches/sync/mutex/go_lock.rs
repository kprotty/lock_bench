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

use std::sync::atomic::{spin_loop_hint, AtomicI32, Ordering};

pub struct Lock {
    state: AtomicI32,
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "go_lock"
    }

    fn new() -> Self {
        Self {
            state: AtomicI32::new(UNLOCKED),
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
                std::thread::yield_now();
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
                    let _ = unsafe {
                        libc::syscall(
                            libc::SYS_futex,
                            &self.state,
                            libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
                            SLEEPING,
                            0
                        )
                    };
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
        let _ = unsafe {
            libc::syscall(
                libc::SYS_futex,
                &self.state,
                libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
                1,
            )
        };
    }
}
