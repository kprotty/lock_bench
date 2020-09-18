#![cfg_attr(not(feature = "std"), no_std)]

pub mod core;
pub mod generic;

#[cfg(any(feature = "os", feature = "std"))]
pub use if_os_or_std::*;

#[cfg(any(feature = "os", feature = "std"))]
mod if_os_or_std {
    pub type ThreadParker = crate::core::SystemThreadParker;

    pub type Mutex<T> = super::generic::Mutex<ThreadParker, T>;
    pub type MutexGuard<'a, T> = super::generic::MutexGuard<'a, ThreadParker, T>; 
}
