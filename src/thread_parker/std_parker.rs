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
    time::Duration,
    sync::atomic::{AtomicBool, Ordering, spin_loop_hint},
};

pub struct Parker {
    notified: AtomicBool,
    thread: thread::Thread,
}

unsafe impl super::ThreadParker for Parker {
    type Instant = std::time::Instant;

    fn new() -> Self {
        Self {
            notified: AtomicBool::new(false),
            thread: thread::current(),
        }
    }

    fn prepare_park(&self) {
        self.notified.store(false, Ordering::Relaxed);
    }

    fn park(&self) {
        while !self.notified.load(Ordering::Acquire) {
            thread::park();
        }
    }

    fn park_until(&self, deadline: Self::Instant) {
        while !self.notified.load(Ordering::Acquire) {
            let now = Self::now();
            if now >= deadline {
                return;
            } else {
                let timeout: Duration = deadline - now;
                thread::park_timeout(timeout);
            }
        }
    }

    fn unpark(&self) {
        self.notified.store(true, Ordering::Release);
        self.thread.unpark();
    }

    fn now() -> Self::Instant {
        Self::Instant::now()
    }

    fn yield_now(iteration: usize) -> bool {
        if iteration <= 3 {
            (0..(1 << iteration)).for_each(|_| spin_loop_hint());
            true
        } else if iteration <= 10 {
            if cfg!(windows) {
                thread::sleep(Duration::new(0, 0));
            } else {
                thread::yield_now();
            }
            true
        } else {
            false
        }
    }
}
