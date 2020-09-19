use core::{ops::Add, time::Duration};

pub unsafe trait ThreadParker: Send {
    fn new() -> Self;

    fn prepare_park(&self);

    fn park(&self);

    fn unpark(&self);
}

pub unsafe trait ThreadParkerTimed: ThreadParker {
    type Instant: Copy + PartialOrd + Add<Duration, Output = Self::Instant>;

    fn now() -> Self::Instant;

    fn park_timeout(&self, deadline: Self::Instant);
}

#[cfg(feature = "std")]
pub use if_std::StdThreadParker;

#[cfg(feature = "std")]
mod if_std {
    use std::{cell::Cell, fmt, thread};

    #[derive(Default)]
    pub struct StdThreadParker(Cell<Option<thread::Thread>>);

    impl fmt::Debug for StdThreadParker {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("ThreadParker").finish()
        }
    }

    unsafe impl Send for StdThreadParker {}

    unsafe impl super::ThreadParker for StdThreadParker {
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
            self.0.replace(None).unwrap().unpark()
        }
    }
}
