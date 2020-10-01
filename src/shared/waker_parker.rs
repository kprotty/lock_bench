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
    cell::Cell,
    task::{Waker, Context, Poll},
    sync::atomic::{AtomicUsize, Ordering},
};

const WAITING: usize = 0;
const UPDATING: usize = 1;
const CONSUMING: usize = 2;
const NOTIFIED: usize = 3;

pub struct WakerParker {
    state: AtomicUsize,
    waker: Cell<Option<Waker>>,
}

unsafe impl Send for WakerParker {}
unsafe impl Sync for WakerParker {}

impl WakerParker {
    pub const fn new() -> Self {
        Self {
            waker: Cell::new(None),
            state: AtomicUsize::new(WAITING),
        }
    }

    pub unsafe fn prepare(&self, ctx: &Context<'_>) {
        if (&*self.waker.as_ptr()).is_none() {
            self.waker.set(Some(ctx.waker().clone()));
            self.state.store(WAITING, Ordering::Relaxed);
        }
    }

    pub unsafe fn poll(&self, ctx: &Context<'_>) -> Result<usize, Poll<()>> {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            match state & 0b11 {
                WAITING => match self.state.compare_exchange_weak(
                    state,
                    UPDATING,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Err(e) => state = e,
                    Ok(_) => {
                        if !(&*self.waker.as_ptr())
                            .as_ref()
                            .expect("WakerParker polled without existing Waker")
                            .will_wake(ctx.waker())
                        {
                            self.waker.set(Some(ctx.waker().clone()));
                        }

                        match self.state.compare_exchange(
                            UPDATING,
                            WAITING,
                            Ordering::Release,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => {
                                return Err(Poll::Ready(()));
                            },
                            Err(state) if state & 0b11 == NOTIFIED => {
                                self.waker.set(None);
                                return Ok(state >> 2);
                            },
                            Err(state) => {
                                unreachable!("invalid state when updating WakerParker: {:?}", state);
                            },
                        }
                    }
                },
                UPDATING => {
                    unreachable!("multiple threads polling the same WakerParker");
                },
                CONSUMING => {
                    return Err(Poll::Pending);
                },
                NOTIFIED => {
                    return Ok(state >> 2);
                },
                _ => unreachable!(),
            }
        }
    }

    pub unsafe fn unpark(&self, token: usize) {
        assert!(token <= (!0usize >> 2), "invalid unpark token");
        let new_state = NOTIFIED | (token << 2);

        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            match state & 0b11 {
                WAITING => match self.state.compare_exchange_weak(
                    state,
                    CONSUMING,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Err(e) => state = e,
                    Ok(_) => {
                        let waker = self
                            .waker
                            .replace(None)
                            .expect("WakerParker waking without a Waker");

                        self.state.store(new_state, Ordering::Release);
                        waker.wake();
                        return;
                    },
                },
                UPDATING => match self.state.compare_exchange_weak(
                    state,
                    new_state,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Err(e) => state = e,
                    Ok(_) => return,
                },
                CONSUMING => {
                    unreachable!("multiple threads waking the same WakerParker");
                },
                NOTIFIED => {
                    unreachable!("WakerParker notified twice");
                },
                _ => unreachable!(),
            }
        }
    }

    pub unsafe fn as_waker<P: Parker>(parker: &P) -> Waker {
        use core::{
            marker::PhantomData,
            task::{RawWaker, RawWakerVTable},
        };
        
        struct ParkWaker<P>(PhantomData<P>);

        impl<P: Parker> ParkWaker<P> {
            const VTABLE: RawWakerVTable = RawWakerVTable::new(
                |ptr| unsafe {
                    (&*(ptr as *const P)).prepare();
                    RawWaker::new(ptr, &Self::VTABLE)
                },
                |ptr| unsafe {
                    (&*(ptr as *const P)).unpark();
                },
                |ptr| unsafe {
                    (&*(ptr as *const P)).unpark();
                },
                |_ptr| {},
            );
        }

        let ptr = parker as *const P as *const ();
        let raw_waker = RawWaker::new(ptr, &ParkWaker::<P>::VTABLE);
        Waker::from_raw(raw_waker)
    }
}