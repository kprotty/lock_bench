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

pub unsafe trait Parker: Sized + Sync + Send {
    type Instant: Copy + PartialOrd + Add<Duration, Output = Self::Instant>;

    fn new() -> Self;

    fn prepare(&self);

    fn park(&self);

    fn park_until(&self, deadline: Self::Instant) -> bool;

    fn unpark(&self);

    fn now() -> Self::Instant;

    fn yield_now(iteration: usize) -> bool;
}

#[cfg(feature = "std")]
mod std_parker;
#[cfg(feature = "std")]
pub use std_parker::StdParker;

#[cfg(feature = "spin")]
mod spin_parker;
#[cfg(feature = "spin")]
pub use spin_parker::SpinParker;
