pub struct Lock {
    inner: std::sync::Mutex<()>,
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "std::sync::Mutex"
    }

    fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(()),
        }
    }

    fn with<F: FnOnce()>(&self, f: F) {
        let _guard = self.inner.lock().unwrap();
        let _ = f();
    }
}
