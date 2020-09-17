pub use super::Lock;

#[cfg(any(windows, unix))]
pub mod os;

pub mod parking_lot;
pub mod spin;
pub mod std;
pub mod usync_core;
