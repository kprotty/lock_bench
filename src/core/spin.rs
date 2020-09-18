use super::cpu::{is_intel, spin_loop_hint};
use core::num::NonZeroU8;

#[derive(Copy, Clone, Debug, Default)]
pub struct Spin {
    iter: u8,
    max: Option<NonZeroU8>,
}

impl Spin {
    pub fn new() -> Self {
        Self { iter: 0, max: None }
    }

    pub fn reset(&mut self) {
        self.iter = 0;
    }

    pub fn yield_now(&mut self) -> bool {
        let max = self.max.unwrap_or_else(|| {
            let max = if is_intel() { 10 } else { 5 };
            self.max = NonZeroU8::new(max);
            self.max.unwrap()
        });

        if self.iter > max.get() {
            false
        } else {
            (0..(1 << self.iter)).for_each(|_| spin_loop_hint());
            self.iter += 1;
            true
        }
    }
}
