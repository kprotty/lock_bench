use core::sync::atomic::spin_loop_hint;

#[derive(Default, Debug)]
pub struct SpinWait(u8);

impl SpinWait {
    pub const fn new() -> Self {
        Self(0)
    }

    pub fn reset(&mut self) {
        *self = Self::new()
    }

    pub fn yield_now(&mut self) -> bool {
        self.0 <= 5 && {
            (0..(1 << self.0)).for_each(|_| spin_loop_hint());
            self.0 += 1;
            true
        }
    }
}
