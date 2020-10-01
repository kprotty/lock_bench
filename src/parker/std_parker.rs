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
    fmt,
    sync::atomic::{spin_loop_hint, AtomicBool, Ordering},
    thread,
};

pub struct StdParker {
    notified: AtomicBool,
    thread: Cell<Option<thread::Thread>>,
}

unsafe impl Send for StdParker {}
unsafe impl Sync for StdParker {}

impl fmt::Debug for StdParker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StdParker")
            .field("notified", &self.notified.load(Ordering::Relaxed))
            .finish()
    }
}

unsafe impl super::Parker for StdParker {
    type Instant = std::time::Instant;

    fn new() -> Self {
        Self {
            notified: AtomicBool::default(),
            thread: Cell::new(None),
        }
    }

    fn prepare(&self) {
        if unsafe { (&*self.thread.as_ptr()).is_none() } {
            self.thread.set(Some(thread::current()));
            self.notified.store(false, Ordering::Relaxed);
        }
    }

    fn park(&self) {
        while !self.notified.load(Ordering::Acquire) {
            thread::park();
        }
    }

    fn park_until(&self, deadline: Self::Instant) -> bool {
        loop {
            if self.notified.load(Ordering::Acquire) {
                return true;
            }

            let now = Self::now();
            if now >= deadline {
                return false;
            }

            let timeout = now - deadline;
            thread::park_timeout(timeout);
        }
    }

    fn unpark(&self) {
        let thread = self
            .thread
            .replace(None)
            .expect("StdParker parked without a thread");

        self.notified.store(true, Ordering::Release);
        thread.unpark();
    }

    fn now() -> Self::Instant {
        Self::Instant::now()
    }

    #[cfg(windows)]
    fn yield_now(iteration: usize) -> bool {
        iteration < 10 && {
            (0..(1 << iteration)).for_each(|_| spin_loop_hint());
            true
        }
    }

    #[cfg(not(windows))]
    fn yield_now(iteration: usize) -> bool {
        iteration < 5 && {
            if iteration < 4 {
                (0..(1 << iteration)).for_each(|_| spin_loop_hint());
            } else {
                thread::yield_now();
            }
            true
        }
    }
}
