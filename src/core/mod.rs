mod lock;

pub use lock::Lock;

pub trait ThreadParker: Sync {
    fn new() -> Self;

    fn prepare_park(&self);

    fn park(&self);

    fn unpark(&self);
}