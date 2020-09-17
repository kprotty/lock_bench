pub struct Lock {
    inner: ulock::core::Lock<()>,
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "ulock::core::Lock"
    }

    fn new() -> Self {
        Self {
            inner: ulock::core::Lock::new(()),
        }
    }

    fn with<F: FnOnce()>(&self, f: F) {
        let _guard = self.inner.lock::<Parker>();
        let _ = f();
    }
}

use std::{cell::Cell, thread};

pub struct Parker(Cell<Option<thread::Thread>>);

unsafe impl Sync for Parker {}

impl ulock::core::ThreadParker for Parker {
    fn new() -> Self {
        Self(Cell::new(None))
    }

    fn prepare_park(&self) {
        self.0.set(Some(thread::current()))
    }

    fn park(&self) {
        thread::park()
    }

    fn unpark(&self) {
        self.0
            .replace(None)
            .expect("prepare_park() not called")
            .unpark()
    }
}
