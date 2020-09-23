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

use crate::thread_parker::ThreadParker as Parker;
use core::{
    marker::PhantomData,
    task::{RawWaker, RawWakerVTable, Waker},
};

pub(crate) struct ParkWaker<P: Parker>(PhantomData<P>);

impl<P: Parker> ParkWaker<P> {
    const VTABLE: RawWakerVTable = RawWakerVTable::new(
        |ptr| unsafe {
            (&*(ptr as *const P)).prepare_park();
            RawWaker::new(ptr, &Self::VTABLE)
        },
        |ptr| unsafe { (&*(ptr as *const P)).unpark() },
        |ptr| unsafe { (&*(ptr as *const P)).unpark() },
        |_ptr| {},
    );

    pub(crate) unsafe fn from(parker: &P) -> Waker {
        let ptr = parker as *const _ as *const ();
        let raw_waker = RawWaker::new(ptr, &Self::VTABLE);
        Waker::from_raw(raw_waker)
    }
}
