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
    cell::UnsafeCell,
    mem::MaybeUninit,
    ptr::{drop_in_place, read, write},
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};

const EMPTY: usize = 0;
const WAITING: usize = 1;
const UPDATING: usize = 2;
const NOTIFYING: usize = 3;
const NOTIFIED: usize = 4;

const MASK: usize = 0b111;
const TOKEN_SHIFT: u32 = MASK.count_ones();

pub struct AsyncParker {
    state: AtomicUsize,
    waker: UnsafeCell<MaybeUninit<Waker>>,
}

unsafe impl Send for AsyncParker {}
unsafe impl Sync for AsyncParker {}

impl AsyncParker {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(EMPTY),
            waker: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    pub unsafe fn prepare_park(&self, ctx: &Context<'_>) {
        let state = self.state.load(Ordering::Relaxed);
        match state & MASK {
            EMPTY | NOTIFIED => {
                write(self.waker.get(), MaybeUninit::new(ctx.waker().clone()));
                self.state.store(WAITING, Ordering::Relaxed);
            },
            WAITING => {},
            _ => {
                unreachable!("invalid AsyncParker state when preparing to park {}", state);
            }
        }
    }

    pub unsafe fn park(&self, ctx: &Context<'_>) -> Poll<usize> {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            match state & MASK {
                EMPTY => unreachable!("tried to park on an empty AsyncParker"),
                WAITING => match self.state.compare_exchange_weak(
                    WAITING,
                    UPDATING,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Err(e) => state = e,
                    Ok(_) => {
                        let waker_ptr = (&mut *self.waker.get()).as_mut_ptr();
                        drop_in_place(waker_ptr);
                        write(waker_ptr, ctx.waker().clone());

                        match self.state.compare_exchange(
                            UPDATING,
                            WAITING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => return Poll::Pending,
                            Err(e) => {
                                state = e;
                                drop_in_place(waker_ptr);
                                assert_eq!(
                                    state & MASK,
                                    NOTIFIED,
                                    "invalid AsyncParker state when updating",
                                );
                            }
                        }
                    }
                },
                UPDATING => unreachable!("multiple threads parking on an AsyncParker"),
                NOTIFYING => return Poll::Pending,
                NOTIFIED => return Poll::Ready(state >> TOKEN_SHIFT),
                _ => unreachable!("invalid AsyncParker state {}", state),
            }
        }
    }

    pub unsafe fn unpark(&self, token: usize) -> Option<Waker> {
        assert!(token <= (!0usize >> TOKEN_SHIFT), "invalid unpark token",);

        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            match state & MASK {
                EMPTY => unreachable!("tried to unpark an empty AsyncParker"),
                WAITING => match self.state.compare_exchange_weak(
                    WAITING,
                    NOTIFYING,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Err(e) => state = e,
                    Ok(_) => {
                        let waker_ptr = (&*self.waker.get()).as_ptr();
                        let waker = read(waker_ptr);
                        self.state
                            .store(NOTIFIED | (token << TOKEN_SHIFT), Ordering::Release);
                        return Some(waker);
                    }
                },
                UPDATING => match self.state.compare_exchange_weak(
                    UPDATING,
                    NOTIFIED | (token << TOKEN_SHIFT),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Err(e) => state = e,
                    Ok(_) => return None,
                },
                NOTIFYING => unreachable!("multiple threads unparking an AsyncParker"),
                NOTIFIED => unreachable!("AsyncParker unparking when already unparked"),
                _ => unreachable!("invalid AsyncParker state when unparking {}", state),
            }
        }
    }
}
