use core::{ops::Sub, time::Duration};

pub trait ThreadParker: Sync {
    type Instant: Copy + Clone + Sub<Duration>;

    fn new() -> Self;

    fn prepare_park(&self);

    fn park(&self);

    fn park_until(&self, instant: Self::Instant);

    fn unpark(&self);

    fn nanotime(&self) -> Self::Instant;
}

#[cfg(feature = "std")]
pub use if_std::*;

#[cfg(feature = "std")]
mod if_std {
    use std::{cell::Cell, thread};

    pub struct StdThreadParker(Cell<Option<thread::Thread>>);

    unsafe impl Sync for StdThreadParker {}

    impl super::ThreadParker for StdThreadParker {
        type Instant = std::time::Instant;

        fn new() -> Self {
            Self(Cell::new(None))
        }

        fn prepare_park(&self) {
            self.0.set(Some(thread::current()))
        }

        fn park(&self) {
            thread::park()
        }

        fn park_until(&self, instant: Self::Instant) {
            thread::park_timeout(instant.saturating_duration_since(self.nanotime()))
        }

        fn unpark(&self) {
            self.0
                .replace(None)
                .expect("prepare_park() not called")
                .unpark()
        }

        fn nanotime(&self) -> Self::Instant {
            Self::Instant::now()
        }
    }
}

#[cfg(feature = "std")]
pub use if_os::*;

#[cfg(feature = "os")]
mod if_os {}
