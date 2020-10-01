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

use core::{
    marker::PhantomPinned,
    cell::{Cell, UnsafeCell},
    ptr::NonNull,
    pin::Pin,
};

#[derive(Debug)]
pub struct Node<T> {
    pub prev: Cell<Option<NonNull<Self>>>,
    pub next: Cell<Option<NonNull<Self>>>,
    pub tail: Cell<Option<NonNull<Self>>>,
    value: UnsafeCell<T>,
    _pinned: PhantomPinned,
}

impl<T: Default> Default for Node<T> {
    fn default() -> Self {
        Self::from(T::default())
    }
}

impl<T> From<T> for Node<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T> Node<T> {
    pub const fn new(value: T) -> Self {
        Self {
            prev: Cell::new(None),
            next: Cell::new(None),
            tail: Cell::new(None),
            value: UnsafeCell::new(value),
            _pinned: PhantomPinned,
        }
    }

    pub fn get(&self) -> *mut T {
        self.value.get()
    }
}

#[derive(Debug)]
pub struct List<T> {
    head: Option<NonNull<Node<T>>>,
}

impl<T> List<T> {
    pub const fn new() -> Self {
        Self { head: None }
    }

    pub fn is_empty(&self) -> bool {
        self.head.is_none()
    }

    pub unsafe fn push_back(&mut self, node: Pin<&Node<T>>) {
        let node = NonNull::from(Pin::get_unchecked(node));
        node.as_ref().next.set(None);
        node.as_ref().tail.set(Some(node));

        if let Some(head) = self.head {
            let tail = head
                .as_ref()
                .tail
                .get()
                .expect("waiter list head without a tail");
                
            node.as_ref().prev.set(Some(tail));
            tail.as_ref().next.set(Some(node));
            head.as_ref().tail.set(Some(node));
        } else {
            self.head = Some(node);
            node.as_ref().prev.set(None);
        }
    }

    #[allow(unused)]
    pub unsafe fn push_front(&mut self, node: Pin<&Node<T>>) {
        let node = NonNull::from(Pin::get_unchecked(node));
        node.as_ref().prev.set(None);
        node.as_ref().next.set(self.head);

        if let Some(head) = std::mem::replace(&mut self.head, Some(node)) {
            node.as_ref().tail.set(head.as_ref().tail.get());
            head.as_ref().prev.set(Some(node));
        }
    }

    pub unsafe fn pop_front(&mut self) -> Option<NonNull<Node<T>>> {
        self.head.map(|head| {
            let node = head;
            assert!(self.try_remove(node));
            node
        })
    }

    #[allow(unused)]
    pub unsafe fn pop_back(&mut self) -> Option<NonNull<Node<T>>> {
        self.head.map(|head| {
            let node = head
                .as_ref()
                .tail
                .get()
                .expect("waiter list head without tail");
            assert!(self.try_remove(node));
            node
        })
    }

    pub unsafe fn try_remove(&mut self, node: Pin<&Node<T>>) -> bool {
        if node.as_ref().tail.get().is_none() {
            return false;
        }

        let head = match self.head {
            Some(head) => head,
            None => return false,
        };

        let prev = node.as_ref().prev.get();
        let next = node.as_ref().next.get();
        if let Some(prev) = prev {
            prev.as_ref().next.set(next);
        }
        if let Some(next) = next {
            next.as_ref().prev.set(prev);
        }

        if head == NonNull::from(node) {
            self.head = next;
            if let Some(new_head) = self.head {
                new_head.as_ref().tail.set(head.as_ref().tail.get());
            }
        } else if head.as_ref().tail.get() == Some(NonNull::from(node)) {
            head.as_ref().tail.set(prev);
        }

        node.as_ref().next.set(None);
        node.as_ref().prev.set(None);
        node.as_ref().tail.set(None);
        true
    }
}
