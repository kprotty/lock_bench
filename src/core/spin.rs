use core::sync::atomic::{spin_loop_hint, AtomicUsize, Ordering};

#[derive(Copy, Clone, Debug, Default)]
pub struct Spin {
    iter: usize,
    max: usize,
}

impl Spin {
    pub fn new() -> Self {
        Self {
            iter: 0,
            max: Self::get_max(),
        }
    }

    pub fn reset(&mut self) {
        self.iter = 0;
    }

    pub fn yield_now(&mut self) -> bool {
        if self.iter > self.max {
            false
        } else {
            (0..(1 << self.iter)).for_each(|_| spin_loop_hint());
            self.iter += 1;
            true
        }
    }

    #[inline]
    fn get_max() -> usize {
        static MAX_SPIN: AtomicUsize = AtomicUsize::new(0);

        let max_spin = MAX_SPIN.load(Ordering::Relaxed);
        if max_spin != 0 {
            return max_spin;
        }

        let max_spin = Self::compute_max();
        MAX_SPIN.store(max_spin, Ordering::Relaxed);
        max_spin
    }

    fn default_max() -> usize {
        6
    }

    #[cold]
    #[cfg(not(any(windows, any(target_arch = "x86", target_arch = "x86_64"))))]
    fn compute_max() -> usize {
        Self::default_max()
    }

    #[cold]
    #[cfg(all(windows, any(target_arch = "x86", target_arch = "x86_64")))]
    fn compute_max() -> usize {
        #[cfg(target_arch = "x86")]
        use core::arch::x86::{CpuidResult, __cpuid};
        #[cfg(target_arch = "x86_64")]
        use core::arch::x86_64::{CpuidResult, __cpuid};

        use core::{slice::from_raw_parts, str::from_utf8_unchecked};

        let is_intel = unsafe {
            let CpuidResult { ebx, ecx, edx, .. } = __cpuid(0);
            let vendor = &[ebx, edx, ecx] as *const _ as *const u8;
            let vendor = from_utf8_unchecked(from_raw_parts(vendor, 3 * 4));
            vendor == "GenuineIntel"
        };

        if is_intel {
            10
        } else {
            Self::default_max()
        }
    }
}
