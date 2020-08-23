// Copyright 2020 kprotty
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use std::{
    cell::{Cell, UnsafeCell},
    fmt,
    ops::{Deref, DerefMut},
    ptr::NonNull,
    sync::atomic::{fence, spin_loop_hint, AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant},
};

const UNLOCKED: usize = 0;
const LOCKED: usize = 1;
const STARVED: usize = 2;
const WAITING: usize = !3;

#[repr(align(4))]
struct Waiter {
    prev: Cell<Option<NonNull<Self>>>,
    next: Cell<Option<NonNull<Self>>>,
    tail: Cell<Option<NonNull<Self>>>,
    parker: Cell<Option<thread::Thread>>,
    state: AtomicUsize,
}

pub struct Mutex<T> {
    state: AtomicUsize,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> From<T> for Mutex<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: fmt::Debug> fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut f = f.debug_struct("Mutex");
        let _ = match self.try_lock() {
            Some(guard) => f.field("value", &&*guard),
            None => f.field("state", &"<locked>"),
        };
        f.finish()
    }
}

impl<T> AsMut<T> for Mutex<T> {
    fn as_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }
}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED),
            value: UnsafeCell::new(value),
        }
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    #[inline]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & (STARVED | LOCKED) != 0 {
                return None;
            }
            match self.state.compare_exchange_weak(
                state,
                state | LOCKED,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(MutexGuard { mutex: self }),
                Err(e) => state = e,
            }
        }
    }

    #[inline]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        if self
            .state
            .compare_exchange_weak(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.lock_slow();
        }
        MutexGuard { mutex: self }
    }

    #[cold]
    fn lock_slow(&self) {
        let waiter = Waiter {
            prev: Cell::new(None),
            next: Cell::new(None),
            tail: Cell::new(None),
            parker: Cell::new(None),
            state: AtomicUsize::new(0),
        };

        let mut spin = 0;
        let mut started = None;
        let mut is_starved = false;
        let mut reset_waiter = true;
        let mut state = self.state.load(Ordering::Relaxed);

        loop {
            let mut new_state = state;
            let head = NonNull::new((state & WAITING) as *mut Waiter);

            if state & (STARVED | LOCKED) == 0 {
                new_state |= LOCKED;
            } else if head.is_none() && spin <= 10 {
                if spin <= 3 {
                    (0..(1 << spin)).for_each(|_| spin_loop_hint());
                } else if cfg!(windows) {
                    thread::sleep(Duration::new(0, 0));
                } else {
                    thread::yield_now();
                }
                spin += 1;
                state = self.state.load(Ordering::Relaxed);
                continue;
            } else {
                new_state = (new_state & !WAITING) | (&waiter as *const _ as usize);
                waiter.next.set(head);
                waiter.tail.set(match head {
                    Some(_) => None,
                    None => Some(NonNull::from(&waiter)),
                });

                if reset_waiter {
                    reset_waiter = false;
                    waiter.prev.set(None);
                    waiter.parker.set(Some(thread::current()));
                    waiter.state.store(LOCKED, Ordering::Relaxed);
                }

                let start_time = started.unwrap_or_else(|| {
                    let now = Instant::now();
                    started = Some(now);
                    now
                });

                is_starved =
                    is_starved || { (Instant::now() - start_time) > Duration::from_micros(1000) };

                if is_starved {
                    new_state |= STARVED;
                }
            }

            if let Err(e) = self.state.compare_exchange_weak(
                state,
                new_state,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                state = e;
                continue;
            }

            if state & LOCKED == 0 {
                return;
            }

            loop {
                match waiter.state.load(Ordering::Acquire) {
                    STARVED => return,
                    LOCKED => thread::park(),
                    UNLOCKED => break,
                    _ => unreachable!(),
                }
            }

            spin = 0;
            reset_waiter = true;
            state = self.state.load(Ordering::Relaxed);
        }
    }

    #[inline]
    pub unsafe fn force_unlock(&self) {
        if let Err(state) =
            self.state
                .compare_exchange(LOCKED, UNLOCKED, Ordering::Release, Ordering::Relaxed)
        {
            self.unlock_slow(state);
        }
    }

    #[cold]
    unsafe fn unlock_slow(&self, mut state: usize) {
        loop {
            let is_starved = state & STARVED != 0;

            let head = NonNull::new_unchecked((state & WAITING) as *mut Waiter);
            let tail = head.as_ref().tail.get().unwrap_or_else(|| {
                let mut current = head;
                loop {
                    let next = current.as_ref().next.get();
                    let next = next.unwrap_or_else(|| std::hint::unreachable_unchecked());
                    next.as_ref().prev.set(Some(current));
                    current = next;
                    if let Some(tail) = current.as_ref().tail.get() {
                        head.as_ref().tail.set(Some(tail));
                        break tail;
                    }
                }
            });

            if let Some(new_tail) = tail.as_ref().prev.get() {
                head.as_ref().tail.set(Some(new_tail));
                if is_starved {
                    fence(Ordering::Release);
                } else {
                    self.state.fetch_and(!LOCKED, Ordering::Release);
                }
            } else if let Err(e) = self.state.compare_exchange_weak(
                state,
                if is_starved { LOCKED } else { UNLOCKED },
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                state = e;
                continue;
            }

            let parker = tail.as_ref().parker.replace(None).unwrap();

            let new_state = if is_starved { STARVED } else { UNLOCKED };
            tail.as_ref().state.store(new_state, Ordering::Release);

            parker.unpark();
            return;
        }
    }
}

pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
}

impl<'a, T> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        unsafe { self.mutex.force_unlock() }
    }
}

impl<'a, T> Deref for MutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.mutex.value.get() }
    }
}

impl<'a, T> DerefMut for MutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.value.get() }
    }
}
