pub struct Lock {
    inner: parking_lot::Mutex<()>,
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "parking_lot::Mutex"
    }

    fn new() -> Self {
        Self {
            inner: parking_lot::Mutex::new(())
        }
    }

    fn with<F: FnOnce()>(&self, f: F) {
        let _guard = self.inner.lock();
        let _ = f();
    }
}