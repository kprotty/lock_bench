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
    cell::{Cell, UnsafeCell},
    marker::PhantomData,
    mem::MaybeUninit,
    ptr::NonNull,
};

#[derive(Debug)]
pub struct Node<T> {
    prev: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    next: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    tail: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    value: UnsafeCell<T>,
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
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
            value: UnsafeCell::new(value),
        }
    }

    pub fn get(&self) -> *mut T {
        self.value.get()
    }
}

#[derive(Debug, Default)]
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

    pub unsafe fn iter<'a>(&'a self) -> ListIter<'a, T> {
        ListIter {
            node: self.head,
            _lifetime: PhantomData,
        }
    }

    pub unsafe fn drain(&mut self) -> ListDrain<'_, T> {
        ListDrain(self)
    }

    pub unsafe fn push(&mut self, node: NonNull<Node<T>>) {
        self.push_back(node)
    }

    pub unsafe fn push_back(&mut self, node: NonNull<Node<T>>) {
        node.as_ref().next.set(MaybeUninit::new(None));
        node.as_ref().tail.set(MaybeUninit::new(Some(node)));

        if let Some(head) = self.head {
            let tail = head.as_ref().tail.get().assume_init().unwrap();
            node.as_ref().prev.set(MaybeUninit::new(Some(tail)));
            tail.as_ref().next.set(MaybeUninit::new(Some(node)));
            head.as_ref().tail.set(MaybeUninit::new(Some(node)));
        } else {
            self.head = Some(node);
            node.as_ref().prev.set(MaybeUninit::new(None));
        }
    }

    pub unsafe fn push_front(&mut self, node: NonNull<Node<T>>) {
        node.as_ref().prev.set(MaybeUninit::new(None));
        node.as_ref().next.set(MaybeUninit::new(self.head));

        if let Some(head) = std::mem::replace(&mut self.head, Some(node)) {
            node.as_ref().tail.set(head.as_ref().tail.get());
            head.as_ref().prev.set(MaybeUninit::new(Some(node)));
        }
    }

    pub unsafe fn pop(&mut self) -> Option<NonNull<Node<T>>> {
        self.pop_front()
    }

    pub unsafe fn pop_front(&mut self) -> Option<NonNull<Node<T>>> {
        self.head.map(|head| {
            let node = head;
            assert!(self.try_remove(node));
            node
        })
    }

    pub unsafe fn pop_back(&mut self) -> Option<NonNull<Node<T>>> {
        self.head.map(|head| {
            let node = head.as_ref().tail.get().assume_init().unwrap();
            assert!(self.try_remove(node));
            node
        })
    }

    pub unsafe fn try_remove(&mut self, node: NonNull<Node<T>>) -> bool {
        let prev = node.as_ref().prev.get().assume_init();
        let next = node.as_ref().next.get().assume_init();
        let head = match self.head {
            Some(head) => head,
            None => return false,
        };

        if let Some(prev) = prev {
            prev.as_ref().next.set(MaybeUninit::new(next));
        }
        if let Some(next) = next {
            next.as_ref().prev.set(MaybeUninit::new(prev));
        }

        if head == NonNull::from(node) {
            self.head = next;
            if let Some(new_head) = self.head {
                new_head.as_ref().tail.set(head.as_ref().tail.get());
            }
        } else if head.as_ref().tail.get().assume_init() == Some(NonNull::from(node)) {
            head.as_ref().tail.set(MaybeUninit::new(prev));
        }

        node.as_ref().next.set(MaybeUninit::new(None));
        node.as_ref().prev.set(MaybeUninit::new(None));
        node.as_ref().tail.set(MaybeUninit::new(None));
        true
    }
}

pub struct ListIter<'a, T> {
    node: Option<NonNull<Node<T>>>,
    _lifetime: PhantomData<&'a List<T>>,
}

impl<'a, T> Iterator for ListIter<'a, T> {
    type Item = NonNull<Node<T>>;

    fn next(&mut self) -> Option<Self::Item> {
        self.node.map(|node| unsafe {
            self.node = node.as_ref().next.get().assume_init();
            node
        })
    }
}

pub struct ListDrain<'a, T>(&'a mut List<T>);

impl<'a, T> Iterator for ListDrain<'a, T> {
    type Item = NonNull<Node<T>>;

    fn next(&mut self) -> Option<Self::Item> {
        unsafe { self.0.pop() }
    }
}
