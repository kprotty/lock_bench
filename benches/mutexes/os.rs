
pub struct Lock {
    inner: os::Lock,
}

unsafe impl Send for Lock {}
unsafe impl Sync for Lock {}

impl super::Lock for Lock {
    fn name() -> &'static str {
        os::Lock::NAME
    }

    fn new() -> Self {
        Self {
            inner: os::Lock::new(),
        }
    }

    fn with<F: FnOnce()>(&self, f: F) {
        self.inner.acquire();
        let _ = f();
        self.inner.release();
    }
}

#[cfg(windows)]
mod os {
    use std::cell::UnsafeCell;

    pub struct Lock(UnsafeCell<usize>);

    #[link(name = "kernel32")]
    extern "system" {
        fn AcquireSRWLockExclusive(p: *mut usize);
        fn ReleaseSRWLockExclusive(p: *mut usize);
    }

    impl Lock {
        pub const NAME: &'static str = "SRWLOCK";

        pub fn new() -> Self {
            Self(UnsafeCell::new(0))
        }

        pub fn acquire(&self) {
            unsafe { AcquireSRWLockExclusive(self.0.get()) }
        }

        pub fn release(&self) {
            unsafe { ReleaseSRWLockExclusive(self.0.get()) }
        }
    }
}

#[cfg(unix)]
mod os {
    use std::{mem::MaybeUninit, cell::UnsafeCell};

    pub struct Lock(Box<UnsafeCell<pthread_mutex_t>>);

    #[repr(C, align(16))]
    struct pthread_mutex_t {
        _opaque: MaybeUninit<[u8; 64]>,
    }

    #[link(name = "c")]
    extern "C" {
        fn pthread_mutex_init(p: *mut pthread_mutex_t, attr: usize) -> i32;
        fn pthread_mutex_destroy(p: *mut pthread_mutex_t) -> i32;
        fn pthread_mutex_lock(p: *mut pthread_mutex_t) -> i32;
        fn pthread_mutex_unlock(p: *mut pthread_mutex_t) -> i32;
    }

    impl Drop for Lock {
        fn drop(&mut self) {
            let rc = unsafe { pthread_mutex_destroy(self.0.get()) };
            assert_eq!(rc, 0);
        }
    }

    impl Lock {
        pub const NAME: &'static str = "pthread_mutex_t";

        pub fn new() -> Self {
            let mutex = Box::new(UnsafeCell::new(pthread_mutex_t {
                _opaque: MaybeUninit::uninit(),
            }));
            let rc = unsafe { pthread_mutex_init(mutex.get(), 0) };
            assert_eq!(rc, 0);
            Self(mutex)
        }

        pub fn acquire(&self) {
            let rc = unsafe { pthread_mutex_lock(self.0.get()) };
            debug_assert_eq!(rc, 0);
        }

        pub fn release(&self) {
            let rc = unsafe { pthread_mutex_unlock(self.0.get()) };
            debug_assert_eq!(rc, 0);
        }
    }
}
