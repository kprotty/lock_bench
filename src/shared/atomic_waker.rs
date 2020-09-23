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
    task::{Context, Waker, Poll},
    sync::atomic::{AtomicUsize, Ordering},
};

const WAITING: usize = 0;
const UPDATING: usize = 1;
const CONSUMING: usize = 2;
const NOTIFIED: usize = 3;

pub struct AtomicWaker {
    state: AtomicUsize,
    waker: Cell<Option<Waker>>,
}

unsafe impl Send for AtomicWaker {}
unsafe impl Sync for AtomicWaker {}

impl AtomicWaker {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(WAITING),
            waker: Cell::new(None),
        }
    }

    pub unsafe fn prepare_poll(&self, ctx: &mut Context<'_>) {
        if (&*self.waker.as_ptr()).is_none() {
            self.waker.set(Some(ctx.waker().clone()));
            self.state.store(WAITING, Ordering::Relaxed);
        }
    }

    pub fn poll(&self, ctx: &mut Context<'_>) -> Poll<usize> {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            match state & 0b11 {
                WAITING => match self.state.compare_exchange_weak(
                    WAITING,
                    UPDATING,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Err(e) => state = e,
                    Ok(_) => {
                        self.waker.set(Some(ctx.waker().clone()));
                        match self.state.compare_exchange(
                            UPDATING,
                            WAKING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => return Poll::Pending,
                            Err(e) => {
                                state = e;
                                self.waker.set(None);
                                assert_eq!(
                                    state & 0b11,
                                    NOTIFIED,
                                    "invalid atomic waker state when updating",
                                );
                            },
                        }
                    },
                },
                UPDATING => {
                    unreachable!("multiple threads polling the atomic waker");
                },
                CONSUMING => {
                    return Poll::Pending;
                },
                NOTIFIED => {
                    return Poll::Ready(state >> 2);
                },
                _ => {
                    unreachable!("invalid atomic waker state {}", state);
                }
            }
        }
    }

    pub fn notify(&self, notification: usize) -> Option<Waker> {
        let notify_state = NOTIFIED | (notification << 2);
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            match state & 0b11 {
                WAITING => match self.state.compare_exchange_weak(
                    WAITING,
                    CONSUMING,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Err(e) => state = e,
                    Ok(_) => {
                        let waker = self
                            .waker
                            .replace(None)
                            .expect("atomic waker waking without a waker");
                        self.state.store(notify_state, Ordering::Release);
                        return Some(waker);
                    },
                },
                UPDATING => match self.state.compare_exchange_weak(
                    UPDATING,
                    notify_state,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Err(e) => state = e,
                    Ok(_) => return None,
                },
                CONSUMING => {
                    unreachable!("multiple threads trying to notify the waker");
                },
                NOTIFIED => {
                    return None;
                },
                _ => {
                    unreachable!("invalid atomic waker state {}", state);
                }
            }
        }
    }
}