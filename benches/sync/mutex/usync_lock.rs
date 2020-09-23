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

type Mutex<T> = usync::base::Lock<usync::SystemParker, T>;

pub struct Lock {
    inner: Mutex<()>,
}

impl super::Lock for Lock {
    fn name() -> &'static str {
        "usync::base::Lock"
    }

    fn new() -> Self {
        Self {
            inner: Mutex::new(()),
        }
    }

    fn with<F: FnOnce()>(&self, f: F) {
        let _guard = self.inner.lock();
        let _ = f();
    }
}
