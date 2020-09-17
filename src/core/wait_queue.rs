use core::{
    cell::Cell,
    mem::MaybeUninit,
    ptr::NonNull,
    hint::unreachable_unchecked,
};

#[inline]
unsafe fn unwrap<T>(x: Option<T>) -> T {
    x.unwrap_or_else(|| unreachable_unchecked!())
}

#[derive(Debug)]
pub struct WaitNode<T> {
    prev: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    next: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    tail: Cell<MaybeUninit<Option<NonNull<Self>>>>,
    value: UnsafeCell<T>,
}

impl<T> From<T> WaitNode<T> {
    fn from(value: T) -> Self {
        Self {
            prev: Cell::new(MaybeUninit::uninit()),
            next: Cell::new(MaybeUninit::uninit()),
            tail: Cell::new(MaybeUninit::uninit()),
            value: UnsafeCell<T>,
        }
    }
}

impl<T> WaitNode<T> {
    pub fn get(&self) -> *mut T {
        self.value.get()
    }
}

#[derive(Debug, Default)]
pub struct WaitQueue<T> {
    head: Option<NonNull<WaitNode<T>>>,
}

impl<T> WaitQueue<T> {
    pub const fn new() -> Self {
        Self { head: None }
    }

    pub unsafe fn push_back(&mut self, node: &WaitNode<T>) {
        let node_ptr = Some(NonNull::from(node));
        node.next.set(MaybeUninit::new(None));
        node.tail.set(MaybeUninit::new(node_ptr));

        if let Some(head) = self.head {
            let tail = unwrap(head.as_ref().tail.get().assume_init());
            node.prev.set(MaybeUninit::new(Some(tail)));
            tail.as_ref().next.set(MaybeUninit::new(node_ptr));
            head.as_ref().tail.set(MaybeUninit::new(node_ptr));
        } else {
            self.head = node_ptr;
            node.prev.set(MaybeUninit::new(None));
        }
    }

    pub unsafe fn push_front(&mut self, node: &WaitNode<T>) {
        let node_ptr = Some(NonNull::from(node));
        node.prev.set(MaybeUninit::new(None));
        node.next.set(MaybeUninit::new(self.head));

        if let Some(head) = std::mem::replace(&mut self.head, node_ptr) {
            node.tail.set(head.as_ref().tail.get());
            head.as_ref().prev.set(MaybeUninit::new(node_ptr));
        }
    }

    pub unsafe fn pop_front(&mut self) -> Option<NonNull<WaitNode<T>>> {
        self.head.map(|head| {
            let node = head;
            assert!(self.try_remove(node.as_ref()));
            node
        })
    }

    pub unsafe fn pop_back(&mut self) -> Option<NonNull<WaitNode<T>>> {
        self.head.map(|head| {
            let node = unwrap(head.as_ref().tail.get().assume_init());
            assert!(self.try_remove(node.as_ref()));
            node
        })
    }

    pub unsafe fn try_remove(&mut self, node: &WaitNode<T>) -> bool {
        let prev = node.prev.get().assume_init();
        let next = node.next.get().assume_init();
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

        node.next.set(MaybeUninit::new(None));
        node.prev.set(MaybeUninit::new(None));
        node.tail.set(MaybeUninit::new(None));
        true
    }
}