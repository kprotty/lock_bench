#![cfg_attr(not(feature = "std"), no_std)]

pub mod list;
mod lock;
mod mutex;
mod spin_wait;
mod thread_parker;

pub use lock::{Lock as RawLock, LockFuture as RawLockFuture, LockGuard as RawLockGuard};
pub use mutex::{
    Mutex as RawMutex, MutexGuard as RawMutexGuard, MutexLockFuture as RawMutexLockFuture,
};
pub use spin_wait::SpinWait;
pub use thread_parker::{ThreadParker, ThreadParkerTimed};

#[cfg(feature = "std")]
pub use if_std::*;

#[cfg(feature = "std")]
mod if_std {
    pub type Parker = super::thread_parker::if_std::Parker;

    pub type Mutex<T> = super::RawMutex<Parker, T>;
    pub type MutexGuard<'a, T> = super::RawMutexGuard<'a, Parker, T>;
    pub type MutexLockFuture<'a, T> = super::RawMutexLockFuture<'a, Parker, T>;
}
