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

const WAITING: usize = 0;
const NOTIFIED: usize = 1;

#[derive(Debug, Default)]
pub struct SpinParker {
    notified: AtomicUsize,
}

unsafe impl super::Parker for SpinParker {
    type Instant = SpinInstant;

    fn new() -> Self {
        Self::default()
    }

    fn prepare(&self) {
        self.notified.store(WAITING, Ordering::Relaxed);
    }

    fn park(&self) {
        while self.notified.load(Ordering::Acquire) == WAITING {
            spin_loop_hint();
        }
    }

    fn park_until(&self, _deadline: Self::Instant) -> bool {
        self.park();
        true
    }

    fn unpark(&self) {
        self.notified.store(NOTIFIED, Ordering::Release);
    }

    fn now() -> Self::Instant {
        Self::Instant::default()
    }

    fn yield_now(_iteration: usize) -> bool {
        spin_loop_hint();
        false
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct SpinInstant {}

impl PartialOrd for SpinInstant {
    fn partial_cmp(&self, _other: &Self) -> Option<CmpOrdering> {
        None
    }
}

impl Add<Duration> for SpinInstant {
    type Output = Self;

    fn add(self, _other: Duration) -> Self {
        self
    }
}
