pub struct Lock {
    inner: usync::Mutex<()>,
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "usync::Mutex"
    }

    fn new() -> Self {
        Self {
            inner: usync::Mutex::new(()),
        }
    }

    fn with<F: FnOnce()>(&self, f: F) {
        let _guard = self.inner.lock();
        let _ = f();
    }
}
