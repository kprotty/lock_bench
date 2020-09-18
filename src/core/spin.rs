use core::sync::atomic::spin_loop_hint;

#[derive(Copy, Clone, Debug, Default)]
pub struct Spin {
    iter: usize,
    max: Option<usize>,
}

impl Spin {
    pub fn new() -> Self {
        Self {
            iter: 0,
            max: None,
        }
    }

    pub fn reset(&mut self) {
        self.iter = 0;
    }

    pub fn yield_now(&mut self) -> bool {
        let max = self.max.unwrap_or_else(|| {
            let max = if super::is_intel() { 10 } else { 10 };
            self.max = Some(max);
            max 
        });

        if self.iter > max {
            false
        } else {
            (0..(1 << self.iter)).for_each(|_| spin_loop_hint());
            self.iter += 1;
            true
        }
    }
}
