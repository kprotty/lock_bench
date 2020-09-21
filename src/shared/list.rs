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
    cell::{Cell, UnsafeCell},
    mem::MaybeUninit,
    ptr::NonNull,
};

pub(crate) struct Node<T> {
    pub(super) prev: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    pub(super) next: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    pub(super) tail: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    value: UnsafeCell<T>,
}

pub(crate) struct List<T> {
    head: Option<NonNull<Node<T>>>,
}
