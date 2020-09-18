use std::{
    thread,
    cell::Cell,
    ptr::NonNull,
    hint::unreachable_unchecked,
    sync::atomic::{spin_loop_hint, AtomicUsize, AtomicU8, AtomicBool, Ordering},
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
    acquire: Cell<bool>,
    thread: thread::Thread,
    notified: AtomicBool,
}

pub struct Lock {
    state: AtomicUsize,
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "experimental"
    }

    fn new() -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED as usize),
        }
    }

    fn with<F: FnOnce()>(&self, f: F) {
        self.acquire();
        let _ = f();
        self.release();
    }
}

impl Lock {
    #[inline]
    fn byte_state(&self) -> &AtomicU8 {
        unsafe { &*(&self.state as *const _ as *const _) }
    }

    #[inline]
    fn acquire(&self) {
        if self.byte_state().swap(LOCKED, Ordering::Acquire) != UNLOCKED {
            self.acquire_slow();
        }
    }

    #[cold]
    fn acquire_slow(&self) {
        let mut spin = 0;
        let max_spin = 10;
        let waiter = Waiter {
            prev: Cell::new(None),
            next: Cell::new(None),
            tail: Cell::new(None),
            thread: thread::current(),
            acquire: Cell::new(false),
            notified: AtomicBool::new(false),
        };

        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & (LOCKED as usize) == 0 {
                if self.byte_state().swap(LOCKED, Ordering::Acquire) == UNLOCKED {
                    return;
                } else {
                    spin_loop_hint();
                    state = self.state.load(Ordering::Relaxed);
                    continue;
                }
            }

            let head = NonNull::new((state & WAITING) as *mut Waiter);
            if head.is_none() && spin <= max_spin {
                (0..(1 << spin)).for_each(|_| spin_loop_hint());
                spin += 1;
                state = self.state.load(Ordering::Relaxed);
                continue;
            }

            waiter.next.set(head);
            waiter.tail.set(match head {
                None => Some(NonNull::from(&waiter)),
                Some(_) => None,
            });

            if let Err(e) = self.state.compare_exchange_weak(
                state,
                (&waiter as *const _ as usize) | (state & !WAITING),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                state = e;
                continue;
            }

            while !waiter.notified.load(Ordering::Acquire) {
                thread::park();
            }

            spin = 0;
            waiter.prev.set(None);
            waiter.notified.store(false, Ordering::Relaxed);
            state = match waiter.acquire.replace(false) {
                true => self.state.fetch_and(!WAKING, Ordering::Relaxed) & !WAKING,
                _ => self.state.load(Ordering::Relaxed),
            };
        }
    }

    #[inline]
    fn release(&self) {
        self.byte_state().store(UNLOCKED, Ordering::Release);

        if self.state.load(Ordering::Relaxed) != (UNLOCKED as usize) {
            self.release_slow();
        }
    }

    #[cold]
    fn release_slow(&self) {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if (state & WAITING == 0) || (state & ((LOCKED as usize) | WAKING) != 0) {
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

        state |= WAKING;
        'outer: loop {
            let (head, tail) = unsafe {
                let head = NonNull::new_unchecked((state & WAITING) as *mut Waiter);
                let tail = head.as_ref().tail.get().unwrap_or_else(|| {
                    let mut current = head;
                    loop {
                        let next = current.as_ref().next.get();
                        let next = next.unwrap_or_else(|| unreachable_unchecked());
                        next.as_ref().prev.set(Some(current));
                        current = next;
                        if let Some(tail) = current.as_ref().tail.get() {
                            head.as_ref().tail.set(Some(tail));
                            break tail;
                        }
                    }
                });
                (&*head.as_ptr(), &*tail.as_ptr())
            };

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

            if let Some(new_tail) = tail.prev.get() {
                head.tail.set(Some(new_tail));
                tail.acquire.set(true);
            } else {
                loop {
                    match self.state.compare_exchange_weak(
                        state,
                        state & (LOCKED as usize),
                        Ordering::Acquire,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => break,
                        Err(e) => state = e,
                    }
                    if state & WAITING != 0 {
                        continue 'outer;
                    }
                }
            }

            tail.notified.store(true, Ordering::Release);
            tail.thread.unpark();
            return;
        }
    }
}
