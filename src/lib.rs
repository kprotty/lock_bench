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

#![cfg_attr(not(feature = "std"), no_std)]

pub mod core;
pub mod generic;

#[cfg(any(feature = "os", feature = "std"))]
pub use if_os_or_std::*;

#[cfg(any(feature = "os", feature = "std"))]
mod if_os_or_std {
    pub type ThreadParker = crate::core::SystemThreadParker;

    pub type Mutex<T> = super::generic::Mutex<ThreadParker, T>;
    pub type MutexGuard<'a, T> = super::generic::MutexGuard<'a, ThreadParker, T>; 
}
