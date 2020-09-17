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
        let _guard = self.inner.lock::<ulock::core::StdThreadParker>();
        let _ = f();
    }
}
