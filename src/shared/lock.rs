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

use super::{
    super::ThreadParker as Parker,
    list::{List, Node},
};
use core::{
    cell::UnsafeCell,
    marker::PhantomPinned,
    sync::atomic::{AtomicUsize, Ordering},
};

pub struct Lock<T> {
    state: AtomicUsize,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for Lock<T> {}
unsafe impl<T: Send> Sync for Lock<T> {}

enum LockState<'a, T> {
    TryLock(&'a Lock<T>),
    Locking(&'a Lock<T>),
    Waiting(&'a Lock<T>),
    Locked,
}

pub struct LockFuture<'a, T> {
    state: LockState<'a, T>,
    _pinned: PhantomPinned,
}

unsafe impl<'a, T: Send> Send for LockFuture<'a, T> {}

pub struct LockGuard<'a, T>(&'a Lock<T>);
