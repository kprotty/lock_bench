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

use std::{
    sync::atomic::{spin_loop_hint, AtomicPtr, AtomicBool, Ordering},
    ptr,
    mem::MaybeUninit,
    cell::UnsafeCell,
};

fn yield_now(spin: usize) {
    if spin < 4 {
        (0..(2 << spin)).for_each(|_| spin_loop_hint());
    } else if cfg!(windows) {
        if spin <= 5 {
            std::thread::yield_now();
        } else {
            std::thread::sleep(std::time::Duration::new(0, 0));
        }
    } else {
        std::thread::yield_now();
    }
}

struct Waiter {
    next: AtomicPtr<Self>,
    notified: AtomicBool,
}

pub struct Lock {
    tail: AtomicPtr<Waiter>
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "mcs_lock"
    }

    fn new() -> Self {
        Self {
            tail: AtomicPtr::new(ptr::null_mut()),
        }
    }

    fn with<F: FnOnce()>(&self, f: F) {
        unsafe {
            let waiter = UnsafeCell::new(MaybeUninit::uninit());
            let waiter_ptr = (&mut *waiter.get()).as_mut_ptr();
            self.acquire(waiter_ptr);
            let _ = f();
            self.release(waiter_ptr);
        }
    }
}

impl Lock {
    #[inline]
    unsafe fn acquire(&self, waiter: *mut Waiter) {
        *(*waiter).next.get_mut() = ptr::null_mut();
        let prev = self.tail.swap(waiter, Ordering::AcqRel);
        if !prev.is_null() {
            Self::acquire_slow(waiter, prev);
        }
    }

    #[cold]
    unsafe fn acquire_slow(waiter: *mut Waiter, prev: *mut Waiter) {
        *(*waiter).notified.get_mut() = false;
        (*prev).next.store(waiter, Ordering::Release);

        let mut spin = 0;
        loop {
            yield_now(spin);
            spin = spin.wrapping_add(1);
            if (*waiter).notified.load(Ordering::Acquire) {
                return;
            }
        }
    }

    #[inline]
    unsafe fn release(&self, waiter: *mut Waiter) {
        if self
            .tail
            .compare_exchange(waiter, ptr::null_mut(), Ordering::Release, Ordering::Relaxed)
            .is_err()
        {
            Self::release_slow(waiter)
        }
    }

    #[cold]
    unsafe fn release_slow(waiter: *mut Waiter) {
        let mut spin = 0;
        loop {
            let next = (*waiter).next.load(Ordering::Relaxed);
            if !next.is_null() {
                (*next).notified.store(true, Ordering::Release);
                return;
            } else {
                yield_now(spin);
                spin = spin.wrapping_add(1);
            }
        }
    }
}
