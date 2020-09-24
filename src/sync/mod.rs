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

mod mutex;
pub use mutex::{
    Mutex as RawMutex, MutexGuard as RawMutexGuard, MutexLockFuture as RawMutexLockFuture,
};

#[cfg(any(feature = "std", feature = "os"))]
pub type Parker = crate::thread_parker::SystemParker;
#[cfg(all(feature = "spin", not(any(feature = "std", feature = "os"))))]
pub type Parker = crate::thread_parker::SpinParker;

pub type Mutex<T> = RawMutex<Parker, T>;
pub type MutexLockFuture<'a, T> = RawMutexLockFuture<'a, Parker, T>;
pub type MutexGuard<'a, T> = RawMutexGuard<'a, Parker, T>;