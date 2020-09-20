type RawLock<T> = ulock::RawLock<T>;

pub struct Lock {
    inner: RawLock<()>,
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "ulock::Lock"
    }

    fn new() -> Self {
        Self {
            inner: RawLock::new(()),
        }
    }

    fn with<F: FnOnce()>(&self, f: F) {
        let _guard = self.inner.lock::<ulock::Parker>();
        let _ = f();
    }
}
