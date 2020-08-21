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
    sync::atomic::{fence, spin_loop_hint, AtomicBool, AtomicU8, AtomicUsize, Ordering},
    thread,
};

const UNLOCKED: u8 = 0;
const LOCKED: u8 = 1;
const WAKING: usize = 1 << 8;
const WAITING: usize = !((1usize << 9) - 1);

#[repr(align(512))]
struct Waiter {
    prev: Cell<Option<NonNull<Self>>>,
    next: Cell<Option<NonNull<Self>>>,
    tail: Cell<Option<NonNull<Self>>>,
    notified: AtomicBool,
    handle: Cell<Option<thread::Thread>>,
}

pub struct Mutex<T> {
    state: AtomicUsize,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Sync> Sync for Mutex<T> {}

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
            state: AtomicUsize::new(UNLOCKED as usize),
            value: UnsafeCell::new(value),
        }
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    fn byte_state(&self) -> &AtomicU8 {
        unsafe { &*(&self.state as *const _ as *const _) }
    }

    #[inline]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.byte_state()
            .compare_exchange(UNLOCKED, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| MutexGuard { mutex: self })
    }

    #[inline]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        if self
            .state
            .compare_exchange_weak(
                UNLOCKED as usize,
                LOCKED as usize,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_err()
        {
            self.lock_slow();
        }
        MutexGuard { mutex: self }
    }

    #[cold]
    fn lock_slow(&self) {
        let mut spin = 0;
        let mut is_waking = false;
        let mut has_event = false;
        let waiter = Waiter {
            prev: Cell::new(None),
            next: Cell::new(None),
            tail: Cell::new(None),
            notified: AtomicBool::new(false),
            handle: Cell::new(None),
        };

        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            let mut new_state = state;
            let head = NonNull::new((state & WAITING) as *mut Waiter);

            if state & (LOCKED as usize) == 0 {
                if is_waking || (state & WAKING == 0) {
                    new_state |= LOCKED as usize;
                }
            } else if head.is_none() && spin <= 10 {
                if spin <= 3 {
                    (0..(1 << spin)).for_each(|_| spin_loop_hint())
                } else if cfg!(windows) {
                    thread::sleep(std::time::Duration::new(0, 0));
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

                if !has_event {
                    has_event = true;
                    waiter.prev.set(None);
                    waiter.handle.set(Some(thread::current()));
                    waiter.notified.store(false, Ordering::Relaxed);
                }
            }

            if is_waking {
                new_state &= !WAKING;
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

            if state & (LOCKED as usize) == 0 {
                return;
            }

            while !waiter.notified.load(Ordering::Relaxed) {
                thread::park();
            }

            spin = 0;
            is_waking = true;
            has_event = false;
            state = self.state.load(Ordering::Relaxed);
        }
    }

    #[inline]
    pub unsafe fn force_unlock(&self) {
        self.byte_state().store(UNLOCKED, Ordering::Release);

        let state = self.state.load(Ordering::Relaxed);
        if (state & WAITING != 0) && (state & (WAKING | (LOCKED as usize)) == 0) {
            self.unlock_slow(state);
        }
    }

    #[cold]
    fn unlock_slow(&self, mut state: usize) {
        loop {
            if (state & WAITING == 0) || (state & (WAKING | (LOCKED as usize)) != 0) {
                return;
            }
            match self.state.compare_exchange_weak(
                state,
                state | WAKING,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(e) => state = e,
            }
        }

        fence(Ordering::Acquire);
        loop {
            unsafe {
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

                if state & (LOCKED as usize) != 0 {
                    match self.state.compare_exchange_weak(
                        state,
                        state & !WAKING,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return,
                        Err(e) => state = e,
                    }
                    continue;
                }

                if let Some(new_tail) = tail.as_ref().prev.get() {
                    head.as_ref().tail.set(Some(new_tail));
                    fence(Ordering::Release);
                } else if let Err(e) = self.state.compare_exchange_weak(
                    state,
                    state & WAKING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    state = e;
                    continue;
                }

                let handle = tail.as_ref().handle.replace(None);
                tail.as_ref().notified.store(true, Ordering::Release);
                if let Some(handle) = handle {
                    handle.unpark();
                }

                return;
            }
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
