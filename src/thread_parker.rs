use core::{ops::Add, time::Duration};

pub unsafe trait ThreadParker: Send + Sync {
    fn new() -> Self;

    fn prepare_park(&self);

    fn park(&self);

    fn unpark(&self);
}

pub unsafe trait ThreadParkerTimed: ThreadParker {
    type Instant: Copy + PartialOrd + Add<Duration, Output = Self::Instant>;

    fn now() -> Self::Instant;

    fn park_until(&self, deadline: Self::Instant);
}

#[cfg(feature = "std")]
pub(crate) mod if_std {
    use std::{
        sync::atomic::{AtomicBool, Ordering},
        thread,
    };

    #[derive(Debug)]
    pub struct Parker {
        notified: AtomicBool,
        thread: thread::Thread,
    }

    unsafe impl super::ThreadParker for Parker {
        fn new() -> Self {
            Self {
                notified: AtomicBool::new(false),
                thread: thread::current(),
            }
        }

        fn prepare_park(&self) {
            self.notified.store(false, Ordering::Relaxed);
        }

        fn park(&self) {
            while !self.notified.load(Ordering::Acquire) {
                thread::park();
            }
        }

        fn unpark(&self) {
            self.notified.store(true, Ordering::Release);
            self.thread.unpark();
        }
    }

    unsafe impl super::ThreadParkerTimed for Parker {
        type Instant = std::time::Instant;

        fn now() -> Self::Instant {
            Self::Instant::now()
        }

        fn park_until(&self, deadline: Self::Instant) {
            while !self.notified.load(Ordering::Acquire) {
                let now = Self::now();
                if deadline <= now {
                    return;
                } else {
                    thread::park_timeout(deadline - now);
                }
            }
        }
    }
}
