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

pub struct Parker {
    notified: AtomicUsize,
}

unsafe impl super::ThreadParker for Parker {
    type Instant = Instant;

    fn new() -> Self {
        compile_error!("TODO")
    }

    fn prepare_park(&self) {
        compile_error!("TODO")
    }

    fn park(&self) {
        compile_error!("TODO")
    }

    fn park_until(&self, _deadline: Self::Instant) {
        compile_error!("TODO")
    }

    fn unpark(&self) {
        compile_error!("TODO")
    }

    fn now() -> Self::Instant {
        compile_error!("TODO")
    }

    fn yield_now(_iteration: usize) -> bool {
        compile_error!("TODO")
    }
}
