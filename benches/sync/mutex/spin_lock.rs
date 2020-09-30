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
    fn acquire(&self) {
        let mut locked = false;
        loop {
            if !locked {
                if self
                    .locked
                    .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    return;
                }
            }
            spin_loop_hint();
            locked = self.locked.load(Ordering::Relaxed);
        }
    }

    fn release(&self) {
        self.locked.store(false, Ordering::Release);
    }
}
