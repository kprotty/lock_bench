// Copyright (c) 2020 kprotty
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

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
