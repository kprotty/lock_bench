use core::{
    slice::from_raw_parts,
    str::from_utf8_unchecked,
    sync::atomic::{AtomicUsize, Ordering},
};

#[inline]
pub fn spin_loop_hint() {
    core::sync::atomic::spin_loop_hint()
}

#[inline]
pub fn is_intel() -> bool {
    IsIntel::get()
}

const IS_UNINIT: usize = 0;
const IS_INTEL: usize = 1;
const IS_NOT_INTEL: usize = 2;

static STATE: AtomicUsize = AtomicUsize::new(IS_UNINIT);

struct IsIntel {}

impl IsIntel {
    #[inline]
    fn get() -> bool {
        let state = match STATE.load(Ordering::Relaxed) {
            IS_UNINIT => Self::get_slow(),
            state => state,
        };
        state == IS_INTEL
    }

    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    #[cold]
    fn get_slow() -> usize {
        let state = IS_NOT_INTEL;
        STATE.store(state, Ordering::Relaxed);
        state
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[cold]
    fn get_slow() -> usize {
        #[cfg(target_arch = "x86")]
        use core::arch::x86::{CpuidResult, __cpuid};
        #[cfg(target_arch = "x86_64")]
        use core::arch::x86_64::{CpuidResult, __cpuid};

        let state = match unsafe {
            let CpuidResult { ebx, ecx, edx, .. } = __cpuid(0);
            let vendor = &[ebx, edx, ecx] as *const _ as *const u8;
            let vendor = from_utf8_unchecked(from_raw_parts(vendor, 3 * 4));
            vendor == "GenuineIntel"
        } {
            true => IS_INTEL,
            _ => IS_NOT_INTEL,
        };

        STATE.store(state, Ordering::Relaxed);
        state
    }
}
