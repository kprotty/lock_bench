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

use core::{ops::Add, time::Duration};

pub trait ThreadParker: Sync {
    type Instant: Copy + PartialOrd + Add<Duration, Output = Self::Instant>;

    fn new() -> Self;

    fn prepare_park(&self);

    fn park(&self);

    fn park_until(&self, instant: Self::Instant);

    fn unpark(&self);

    fn now() -> Self::Instant;
}

#[cfg(feature = "std")]
pub type SystemThreadParker = StdThreadParker;

#[cfg(all(feature = "os", not(feature = "std")))]
pub type SystemThreadParker = OsThreadParker;

#[cfg(feature = "std")]
pub use if_std::StdThreadParker;

#[cfg(feature = "std")]
mod if_std {
    use std::{cell::Cell, thread};

    pub struct StdThreadParker(Cell<Option<thread::Thread>>);

    unsafe impl Sync for StdThreadParker {}

    impl super::ThreadParker for StdThreadParker {
        type Instant = std::time::Instant;

        fn new() -> Self {
            Self(Cell::new(None))
        }

        fn prepare_park(&self) {
            self.0.set(Some(thread::current()))
        }

        fn park(&self) {
            thread::park()
        }

        fn park_until(&self, instant: Self::Instant) {
            thread::park_timeout(instant.saturating_duration_since(Self::now()))
        }

        fn unpark(&self) {
            self.0
                .replace(None)
                .expect("prepare_park() not called")
                .unpark()
        }

        fn now() -> Self::Instant {
            Self::Instant::now()
        }
    }
}

#[cfg(feature = "std")]
pub use if_os::*;

#[cfg(feature = "os")]
mod if_os {}
