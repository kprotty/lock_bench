type Mutex<T> = ulock::Mutex<T>;

pub struct Lock {
    inner: Mutex<()>,
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "ulock::Mutex"
    }

    fn new() -> Self {
        Self {
            inner: Mutex::new(()),
        }
    }

    fn with<F: FnOnce()>(&self, f: F) {
        let _guard = self.inner.lock();
        let _ = f();
    }
}
