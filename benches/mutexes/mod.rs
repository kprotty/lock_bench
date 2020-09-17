pub use super::Lock;

#[cfg(any(windows, unix))]
pub mod os;

pub mod parking_lot;
pub mod ulock_core;
pub mod spin;
pub mod std;
