pub struct Lock {
    inner: usync::core::Lock<()>,
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "usync::core::Lock"
    }

    fn new() -> Self {
        Self {
            inner: usync::core::Lock::new(()),
        }
    }

    fn with<F: FnOnce()>(&self, f: F) {
        let _guard = self.inner.lock::<usync::core::StdThreadParker>();
        let _ = f();
    }
}
