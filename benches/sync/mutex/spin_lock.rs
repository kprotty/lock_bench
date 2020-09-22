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

use std::sync::atomic::{spin_loop_hint, AtomicBool, Ordering};

pub struct Lock {
    locked: AtomicBool,
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "spin_lock"
    }

    fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
        }
    }

    fn with<F: FnOnce()>(&self, f: F) {
        self.acquire();
        let _ = f();
        self.release();
    }
}

impl Lock {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    fn try_acquire(&self) -> bool {
        !self.locked.swap(true, Ordering::Acquire)
    }

    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    fn try_acquire(&self) -> bool {
        self.locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    fn acquire(&self) {
        let mut spin = 0;
        while !self.try_acquire() {
            if spin <= 6 {
                (0..(1 << spin)).for_each(|_| spin_loop_hint());
                spin += 1;
            } else if cfg!(windows) {
                std::thread::sleep(std::time::Duration::new(0, 0));
            } else {
                std::thread::yield_now();
            }
        }
    }

    fn release(&self) {
        self.locked.store(false, Ordering::Release);
    }
}
