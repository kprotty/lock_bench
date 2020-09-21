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

use core::{
    cmp::Ordering as CmpOrdering,
    ops::Add,
    sync::atomic::{spin_loop_hint, AtomicUsize, Ordering},
    time::Duration,
};

pub struct Parker {
    notified: AtomicUsize,
}

unsafe impl super::ThreadParker for Parker {
    type Instant = Instant;

    fn new() -> Self {
        Self {
            notified: AtomicUsize::new(0),
        }
    }

    fn prepare_park(&self) {
        self.notified.store(0, Ordering::Relaxed);
    }

    fn park(&self) {
        while self.notified.load(Ordering::Acquire) == 0 {
            spin_loop_hint();
        }
    }

    fn park_until(&self, _deadline: Self::Instant) {
        self.park()
    }

    fn unpark(&self) {
        self.notified.store(1, Ordering::Release);
    }

    fn now() -> Self::Instant {
        Self::Instant {}
    }

    fn yield_now(_iteration: usize) -> bool {
        spin_loop_hint();
        true
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct Instant;

impl Ord for Instant {
    fn cmp(&self, _other: &Self) -> CmpOrdering {
        CmpOrdering::Equal
    }
}

impl PartialOrd for Instant {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Add<Duration> for Instant {
    type Output = Self;

    fn add(self, _other: Duration) -> Self {
        Self {}
    }
}
