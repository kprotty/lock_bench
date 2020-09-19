pub struct Lock {
    inner: ulock::RawLock<()>,
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "ulock::Lock"
    }

    fn new() -> Self {
        Self {
            inner: ulock::RawLock::new(()),
        }
    }

    fn with<F: FnOnce()>(&self, f: F) {
        let _guard = self.inner.lock::<ulock::StdThreadParker>();
        let _ = f();
    }
}
