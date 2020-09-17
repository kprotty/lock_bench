use std::sync::atomic::{spin_loop_hint, AtomicBool, Ordering};

pub struct Lock {
    locked: AtomicBool,
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "spin_lock"
    }

    fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
        }
    }

    fn with<F: FnOnce()>(&self, f: F) {
        self.acquire();
        let _ = f();
        self.release();
    }
}

impl Lock {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    fn try_acquire(&self) -> bool {
        !self.locked.swap(true, Ordering::Acquire)
    }

    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    fn try_acquire(&self) -> bool {
        self.locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    fn acquire(&self) {
        while !self.try_acquire() {
            spin_loop_hint();
        }
    }

    fn release(&self) {
        self.locked.store(false, Ordering::Release);
    }
}
