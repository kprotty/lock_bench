
#[cfg(any(windows, target_os = "linux"))]
pub use custom::Instant;
#[cfg(not(any(windows, target_os = "linux")))]
pub use std::time::Instant;


mod custom {
    use std::{
        convert::TryInto,
        ops::{Add, Sub},
        time::Duration,
    };

    #[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug, Hash)]
    pub struct Instant(u64);

    impl Add<Duration> for Instant {
        type Output = Self;

        fn add(self, time: Duration) -> Self {
            let nanos: u64 = time.as_nanos().try_into().unwrap();
            Self(self.0.wrapping_add(nanos))
        }
    }

    impl Sub<Self> for Instant {
        type Output = Duration;

        fn sub(self, other: Self) -> Duration {
            assert!(self.0 >= other.0);
            Duration::from_nanos(self.0 - other.0)
        }
    }

    impl Instant {
        pub fn now() -> Self {
            Self(unsafe {
                if Self::IS_ACTUALLY_MONOTONIC {
                    Self::nanotime()
                } else {
                    Self::now_monotonic()
                }
            })
        }
    }

    #[cfg(target_pointer_width = "64")]
    impl Instant {
        unsafe fn now_monotonic() -> u64 {
            use std::sync::atomic::{AtomicU64, Ordering};
            static LAST: AtomicU64 = AtomicU64::new(0);

            let ts = Self::nanotime();
            let mut last = LAST.load(Ordering::Relaxed);

            loop {
                if last >= ts {
                    return last;
                } else if let Err(e) =
                    LAST.compare_exchange_weak(last, ts, Ordering::Relaxed, Ordering::Relaxed)
                {
                    last = e;
                } else {
                    return ts;
                }
            }
        }
    }

    #[cfg(not(target_pointer_width = "64"))]
    impl Instant {
        unsafe fn now_monotonic() -> u64 {
            use std::{
                sync::atomic::{spin_loop_hint, AtomicBool, Ordering},
                thread,
            };
            static mut LAST: u64 = 0;
            static LOCK: AtomicBool = AtomicBool::new(false);

            let mut spin = 0;
            while LOCK.swap(true, Ordering::Acquire) {
                if spin <= 5 {
                    (0..(1 << spin)).for_each(|_| spin_loop_hint());
                    spin += 1;
                } else if cfg!(windows) {
                    thread::sleep(Duration::new(0, 0));
                } else {
                    thread::yield_now();
                }
            }

            let mut ts = Self::nanotime();
            if ts >= LAST {
                LAST = ts;
            } else {
                ts = LAST;
            }

            LOCK.store(false, Ordering::Release);
            ts
        }
    }

    #[cfg(windows)]
    impl Instant {
        const IS_ACTUALLY_MONOTONIC: bool = false;

        unsafe fn nanotime() -> u64 {
            use std::{
                mem::transmute,
                sync::atomic::{AtomicU8, Ordering},
            };

            #[link(name = "kernel32")]
            extern "system" {
                fn QueryPerformanceFrequency(p: &mut i64) -> bool;
                fn QueryPerformanceCounter(p: &mut i64) -> bool;
            }

            let frequency: u64 = transmute({
                static FREQ_STATE: AtomicU8 = AtomicU8::new(0);
                static mut FREQ: i64 = 0;

                if FREQ_STATE.load(Ordering::Acquire) == 2 {
                    FREQ
                } else {
                    let mut freq = 0i64;
                    assert!(QueryPerformanceFrequency(&mut freq));
                    if FREQ_STATE
                        .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
                        .is_ok()
                    {
                        FREQ = freq;
                        FREQ_STATE.store(2, Ordering::Release);
                    }
                    freq
                }
            });

            let mut counter = 0i64;
            assert!(QueryPerformanceCounter(&mut counter));
            let counter: u64 = transmute(counter);

            counter.wrapping_mul(1_000_000_000) / frequency
        }
    }

    #[cfg(target_os = "linux")]
    impl Instant {
        #[cfg(all(target_os = "linux", any(target_arch = "arm", target_arch = "s390x")))]
        const IS_ACTUALLY_MONOTONIC: bool = false;

        #[cfg(not(all(target_os = "linux", any(target_arch = "arm", target_arch = "s390x"))))]
        const IS_ACTUALLY_MONOTONIC: bool = true;

        unsafe fn nanotime() -> u64 {
            let mut ts = std::mem::MaybeUninit::uninit();
            let _ = libc::clock_gettime(libc::CLOCK_MONOTONIC, ts.as_mut_ptr());
            let ts = ts.assume_init();
            ((ts.tv_sec as u64) * (1_000_000_000)) + (ts.tv_nsec as u64)
        }
    }

}

