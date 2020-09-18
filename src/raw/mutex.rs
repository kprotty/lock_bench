use core::{
    cell::{Cell, UnsafeCell},
    fmt,
    future::Future,
    hint::unreachable_unchecked,
    marker::{PhantomData, PhantomPinned},
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
    pin::Pin,
    ptr::{drop_in_place, read, write, NonNull},
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

use core::{ThreadParker, WaitNode, WaitQueue, RawMutex};

enum WaiterState {
    Empty,
    Waiting(Waker),
    Notified { handoff: bool },
}

type Waiter = WaitNode<WaiterState>;
type WaiterQueue = WaitQueue<WaiterState>;

pub struct RawMutex<P, T> {
    state: AtomicUsize,
    queue: RawMutex<WaiterQueue>,
    value: UnsafeCell<T>,
}

impl<T: fmt::Debug> fmt::Debug for RawMutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut f = f.debug_struct("RawMutex");
        let f = match self.try_lock() {
            Some(guard) => f.field("value", &&*guard),
            None => f.field("state", &"<locked>"),
        };
        f.finish()
    }
}

impl<T: Default> Default for RawMutex<T> {
    fn default() -> Self {
        Self::from(T::default())
    }
}

impl<T> From<T> for RawMutex<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T> AsMut<T> for RawMutex<T> {
    fn as_mut(&mut self) -> &mut T {
        unsafe { &mut *self.value.get() }
    }
}

unsafe impl<T: Send> Send for RawMutex<T> {}
unsafe impl<T: Send> Sync for RawMutex<T> {}

const UNLOCKED: usize = 0;
const LOCKED: usize = 1 << 0;
const PARKED: usize = 1 << 1;

impl<T> RawMutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicUsize::new(UNLOCKED),
            queue: RawMutex::new(WaiterQueue::new()),
            value: UnsafeCell::new(value),
        }
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    #[inline]
    pub fn try_lock(&self) -> Option<RawMutexGuard<'_, T>> {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & LOCKED != 0 {
                return None;
            }
            match self.state.compare_exchange_weak(
                state,
                state | LOCKED,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(RawMutexGuard(self)),
                Err(e) => state = e,
            }
        }
    }

    #[inline]
    pub fn lock_fast(&self) -> Result<RawMutexGuard<'_, T>, RawMutexFuture<'_, T>> {
        match self.state.compare_exchange_weak(
            UNLOCKED,
            LOCKED,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            Ok(_) => Ok(RawMutexGuard(self)),
            Err(_) => Err(RawMutexFuture::new(self)),
        }
    }

    pub fn lock_async(&self) -> RawMutexAsyncFuture<'_, T> {
        RawMutexAsyncFuture(Some(AsyncState::TryLock(self)))
    }

    #[inline]
    pub fn lock<P: ThreadParker>(&self) -> RawMutexGuard<'_, T> {
        match self.lock_fast() {
            Ok(guard) => guard,
            Err(future) => unsafe { Self::lock_slow::<P>(future) },
        }
    }

    #[cold]
    unsafe fn lock_slow<'a, P: ThreadParker>(future: RawMutexFuture<'a, T>) -> RawMutexGuard<'a, T> {
        struct ParkWaker<P: ThreadParker>(PhantomData<*mut P>);
        impl<P: ThreadParker> ParkWaker<P> {
            const VTABLE: RawWakerVTable =
                RawWakerVTable::new(Self::clone, Self::wake, Self::wake, |_| {});

            unsafe fn clone(ptr: *const ()) -> RawWaker {
                (&*(ptr as *const P)).prepare_park();
                RawWaker::new(ptr, &Self::VTABLE)
            }

            unsafe fn wake(ptr: *const ()) {
                (&*(ptr as *const P)).unpark()
            }
        }

        let mut future = future;
        let parker = P::new();
        let parker_ptr = &parker as *const _ as *const ();
        let waker = Waker::from_raw(RawWaker::new(parker_ptr, &ParkWaker::<P>::VTABLE));

        loop {
            let mut context = Context::from_waker(&waker);
            match Pin::new_unchecked(&mut future).poll(&mut context) {
                Poll::Pending => parker.park(),
                Poll::Ready(guard) => return guard,
            }
        }
    }

    #[inline]
    pub unsafe fn unlock(&self) {
        self.unlock_fast(false);
    }

    #[inline]
    pub unsafe fn unlock_fair(&self) {
        self.unlock_fast(true)
    }

    #[inline]
    unsafe fn unlock_fast(&self, is_fair: bool) {
        if let Err(_) = self.state.compare_exchange(
            LOCKED,
            UNLOCKED,
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            self.unlock_slow(is_fair);
        }
    }

    #[cold]
    unsafe fn unlock_slow(&self, is_fair: bool) {
        
    }
}

enum AsyncState<'a, T> {
    TryLock(&'a RawMutex<T>),
    Poll(RawMutexFuture<'a, T>),
}

pub struct RawMutexAsyncFuture<'a, T>(Option<AsyncState<'a, T>>);

impl<'a, T> Drop for RawMutexAsyncFuture<'a, T> {
    fn drop(&mut self) {
        if matches!(self.0, Some(AsyncState::Poll(_))) {
            unreachable!("RawMutexAsyncFuture does not support cancellation");
        }
    }
}

impl<'a, T> Future for RawMutexAsyncFuture<'a, T> {
    type Output = RawMutexGuard<'a, T>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe {
            let mut_self = Pin::into_inner_unchecked(self);
            loop {
                let guard = match mut_self.0 {
                    None => unreachable!("RawMutexAsyncFuture polled after completion"),
                    Some(AsyncState::TryLock(lock)) => match lock.lock_fast() {
                        Ok(guard) => guard,
                        Err(future) => {
                            mut_self.0 = Some(AsyncState::Poll(future));
                            continue;
                        }
                    },
                    Some(AsyncState::Poll(ref mut future)) => {
                        match Pin::new_unchecked(future).poll(ctx) {
                            Poll::Pending => return Poll::Pending,
                            Poll::Ready(guard) => guard,
                        }
                    }
                };

                mut_self.0 = None;
                return Poll::Ready(guard);
            }
        }
    }
}

pub struct RawMutexFuture<'a, T> {
    lock: Option<&'a RawMutex<T>>,
    waiter: Waiter,
    _pinned: PhantomPinned,
}

impl<'a, T> RawMutexFuture<'a, T> {
    fn new(lock: &'a RawMutex<T>) -> Self {
        Self {
            lock: Some(lock),
            waiter: Waiter::from(WaiterState::Empty),
            _pinned: PhantomPinned,
        }
    }
}

impl<'a, T> Drop for RawMutexFuture<'a, T> {
    fn drop(&mut self) {
        if self.lock.is_some() {
            unreachable!("RawMutexFuture does not support cancellation");
        }
    }
}

impl<'a, T> Future for RawMutexFuture<'a, T> {
    type Output = RawMutexGuard<'a, T>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe {
            let mut_self = Pin::into_inner_unchecked(self);
            let lock = mut_self.lock.expect("RawMutexFuture polled after completion");
            
            compile_error!("todo")
        }
    }
}

pub struct RawMutexGuard<'a, T>(&'a RawMutex<T>);

impl<'a, T> RawMutexGuard<'a, T> {
    fn unlock_fair(self: Self) {
        unsafe { self.0.unlock_fair() }
    }
}

impl<'a, T> Drop for RawMutexGuard<'a, T> {
    fn drop(&mut self) {
        unsafe { self.0.unlock() }
    }
}

impl<'a, T: fmt::Debug> fmt::Debug for RawMutexGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(f)
    }
}

impl<'a, T> Deref for RawMutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.0.value.get() }
    }
}

impl<'a, T> DerefMut for RawMutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.0.value.get() }
    }
}
