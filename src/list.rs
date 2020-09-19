use core::{
    cell::{Cell, UnsafeCell},
    fmt,
    ptr::NonNull,
};

pub struct Node<T> {
    prev: Cell<Option<NonNull<Self>>>,
    next: Cell<Option<NonNull<Self>>>,
    tail: Cell<Option<NonNull<Self>>>,
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

impl<T: fmt::Debug> fmt::Debug for Node<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = unsafe { &*self.value.get() };
        value.fmt(f)
    }
}

impl<T> Node<T> {
    pub const fn new(value: T) -> Self {
        Self {
            prev: Cell::new(None),
            next: Cell::new(None),
            tail: Cell::new(None),
            value: UnsafeCell::new(value),
        }
    }

    pub fn get(&self) -> *mut T {
        self.value.get()
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }
}

pub struct List<T> {
    head: Option<NonNull<Node<T>>>,
}

impl<T: fmt::Debug> fmt::Debug for List<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("List")
            .field("is_empty", &self.is_empty())
            .finish()
    }
}

impl<T: Default> Default for List<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> List<T> {
    pub const fn new() -> Self {
        Self { head: None }
    }

    pub fn is_empty(&self) -> bool {
        self.head.is_none()
    }

    pub unsafe fn push_back(&mut self, node: NonNull<Node<T>>) {
        node.as_ref().next.set(None);
        node.as_ref().tail.set(Some(node));

        if let Some(head) = self.head {
            let tail = head.as_ref().tail.get().unwrap();
            tail.as_ref().next.set(Some(node));
            head.as_ref().tail.set(Some(node));
            node.as_ref().prev.set(Some(tail));
        } else {
            self.head = Some(node);
            node.as_ref().prev.set(None);
        }
    }

    pub unsafe fn push_front(&mut self, node: NonNull<Node<T>>) {
        node.as_ref().prev.set(None);
        node.as_ref().tail.set(Some(node));

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

    pub unsafe fn pop_back(&mut self) -> Option<NonNull<Node<T>>> {
        self.head.map(|head| {
            let node = head.as_ref().tail.get().unwrap();
            assert!(self.try_remove(node));
            node
        })
    }

    pub unsafe fn try_remove(&mut self, node: NonNull<Node<T>>) -> bool {
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

        if head == node {
            self.head = next;
            if let Some(new_head) = self.head {
                new_head.as_ref().tail.set(head.as_ref().tail.get());
            }
        } else if head.as_ref().tail.get() == Some(node) {
            head.as_ref().tail.set(prev);
        }

        node.as_ref().next.set(None);
        node.as_ref().prev.set(None);
        node.as_ref().tail.set(None);
        true
    }
}
