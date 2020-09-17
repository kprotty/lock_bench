pub use super::Lock;

#[cfg(windows, unix)]
pub mod os;

pub mod parking_lot;
pub mod spin;
pub mod std;
