mod lock;
mod spin;

pub use lock::Lock;
pub use spin::Spin;

pub trait ThreadParker: Sync {
    fn new() -> Self;

    fn prepare_park(&self);

    fn park(&self);

    fn unpark(&self);
}
