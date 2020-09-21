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

pub unsafe trait ThreadParker: Sync {
    type Instant: Copy + PartialOrd + Add<Duration, Output = Self::Instant>;

    fn new() -> Self;

    fn prepare_park(&self);

    fn park(&self);

    fn park_until(&self, deadline: Self::Instant);

    fn unpark(&self);

    fn now() -> Self::Instant;

    fn yield_now(iteration: usize) -> bool;
}

mod spin_parker;
pub type SpinThreadParker = spin_parker::Parker;

#[cfg(feature = "std")]
mod std_parker;
#[cfg(feature = "std")]
pub type StdThreadParker = std_parker::Parker;

#[cfg(feature = "os")]
mod os_parker;
#[cfg(feature = "os")]
pub type OsThreadParker = os_parker::Parker;

#[cfg(feature = "std")]
pub type SystemParker = StdThreadParker;
#[cfg(all(feature = "os", not(feature = "std")))]
pub type SystemParker = OsThreadParker;

#[cfg(any(feature = "std", feature = "os"))]
pub type DefaultParker = SystemParker;
#[cfg(not(any(feature = "std", feature = "os")))]
pub type DefaultParker = SpinThreadParker;
