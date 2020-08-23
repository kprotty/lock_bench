// Copyright 2016 Amanieu d'Antras
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

mod args;
use crate::args::ArgRange;

use std::cell::UnsafeCell;
use std::{
    sync::{
        atomic::{spin_loop_hint, AtomicBool, Ordering},
        Arc, Barrier,
    },
    thread,
    time::Duration,
};

trait Mutex<T> {
    fn new(v: T) -> Self;
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R;
    fn name() -> &'static str;
}

impl<T> Mutex<T> for std::sync::Mutex<T> {
    fn new(v: T) -> Self {
        Self::new(v)
    }
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.lock().unwrap())
    }
    fn name() -> &'static str {
        "std::sync::Mutex"
    }
}

impl<T> Mutex<T> for parking_lot::Mutex<T> {
    fn new(v: T) -> Self {
        Self::new(v)
    }
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.lock())
    }
    fn name() -> &'static str {
        "parking_lot::Mutex"
    }
}

#[repr(align(16))]
struct SystemLock(UnsafeCell<[u64; 8]>);

#[cfg(unix)]
#[link(name = "c")]
extern "C" {
    fn pthread_mutex_init(p: usize, a: usize) -> i32;
    fn pthread_mutex_destroy(p: usize) -> i32;
    fn pthread_mutex_lock(p: usize) -> i32;
    fn pthread_mutex_unlock(p: usize) -> i32;
}

#[cfg(unix)]
impl Drop for SystemLock {
    fn drop(&mut self) {
        let _ = unsafe { pthread_mutex_destroy(&self.0 as *const _ as usize) };
    }
}

#[cfg(unix)]
impl SystemLock {
    unsafe fn init(&mut self) {
        let _ = pthread_mutex_init(&mut self.0 as *mut _ as usize, 0);
    }

    unsafe fn lock(&self) {
        let _ = pthread_mutex_lock(&self.0 as *const _ as usize);
    }

    unsafe fn unlock(&self) {
        let _ = pthread_mutex_unlock(&self.0 as *const _ as usize);
    }

    fn lock_name() -> &'static str {
        "pthread_mutex_t"
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn AcquireSRWLockExclusive(p: usize);
    fn ReleaseSRWLockExclusive(p: usize);
}

#[cfg(windows)]
impl SystemLock {
    unsafe fn init(&mut self) {}

    unsafe fn lock(&self) {
        AcquireSRWLockExclusive(&self.0 as *const _ as usize);
    }

    unsafe fn unlock(&self) {
        ReleaseSRWLockExclusive(&self.0 as *const _ as usize);
    }

    fn lock_name() -> &'static str {
        "SRWLOCK"
    }
}

struct SystemMutex<T>(UnsafeCell<T>, Box<SystemLock>);

unsafe impl<T: Send> Sync for SystemMutex<T> {}

impl<T> Mutex<T> for SystemMutex<T> {
    fn new(v: T) -> Self {
        let mut mutex = Self(
            UnsafeCell::new(v),
            Box::new(SystemLock(UnsafeCell::new([0; 8]))),
        );
        unsafe { mutex.1.init() };
        mutex
    }

    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        unsafe {
            self.1.lock();
            let res = f(&mut *self.0.get());
            self.1.unlock();
            res
        }
    }

    fn name() -> &'static str {
        SystemLock::lock_name()
    }
}

impl<T> Mutex<T> for lock_bench::eventually_fair::Mutex<T> {
    fn new(v: T) -> Self {
        Self::new(v)
    }
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.lock())
    }
    fn name() -> &'static str {
        "custom::eventually_fair::Mutex"
    }
}

impl<T> Mutex<T> for lock_bench::throughput::Mutex<T> {
    fn new(v: T) -> Self {
        Self::new(v)
    }
    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.lock())
    }
    fn name() -> &'static str {
        "custom::throughput::Mutex"
    }
}

struct SpinLock<T>(UnsafeCell<T>, AtomicBool);

unsafe impl<T: Send> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    #[inline]
    fn acquire(&self) {
        if self
            .1
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.acquire_slow();
        }
    }

    #[cold]
    fn acquire_slow(&self) {
        let mut spin = 0usize;
        let mut locked = true;

        loop {
            if !locked {
                match self.1.compare_exchange_weak(
                    false,
                    true,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(e) => locked = e,
                }
                spin = 0;
                continue;
            }

            if spin <= 3 {
                (0..(1 << spin)).for_each(|_| spin_loop_hint());
            } else if cfg!(windows) {
                std::thread::sleep(std::time::Duration::new(0, 0));
            } else {
                std::thread::yield_now();
            }

            locked = self.1.load(Ordering::Relaxed);
        }
    }

    #[inline]
    fn release(&self) {
        self.1.store(false, Ordering::Relaxed);
    }
}

impl<T> Mutex<T> for SpinLock<T> {
    fn new(v: T) -> Self {
        Self(UnsafeCell::new(v), AtomicBool::new(false))
    }

    fn lock<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        self.acquire();
        let res = f(unsafe { &mut *self.0.get() });
        self.release();
        res
    }

    fn name() -> &'static str {
        "spin_lock"
    }
}

fn run_benchmark<M: Mutex<f64> + Send + Sync + 'static>(
    num_threads: usize,
    work_per_critical_section: usize,
    work_between_critical_sections: usize,
    seconds_per_test: usize,
) -> Vec<usize> {
    let lock = Arc::new(([0u8; 300], M::new(0.0), [0u8; 300]));
    let keep_going = Arc::new(AtomicBool::new(true));
    let barrier = Arc::new(Barrier::new(num_threads));
    let mut threads = vec![];
    for _ in 0..num_threads {
        let barrier = barrier.clone();
        let lock = lock.clone();
        let keep_going = keep_going.clone();
        threads.push(thread::spawn(move || {
            let mut local_value = 0.0;
            let mut value = 0.0;
            let mut iterations = 0usize;
            barrier.wait();
            while keep_going.load(Ordering::Relaxed) {
                lock.1.lock(|shared_value| {
                    for _ in 0..work_per_critical_section {
                        *shared_value += value;
                        *shared_value *= 1.01;
                        value = *shared_value;
                    }
                });
                for _ in 0..work_between_critical_sections {
                    local_value += value;
                    local_value *= 1.01;
                    value = local_value;
                }
                iterations += 1;
            }
            (iterations, value)
        }));
    }

    thread::sleep(Duration::from_secs(seconds_per_test as u64));
    keep_going.store(false, Ordering::Relaxed);
    threads.into_iter().map(|x| x.join().unwrap().0).collect()
}

fn run_benchmark_iterations<M: Mutex<f64> + Send + Sync + 'static>(
    num_threads: usize,
    work_per_critical_section: usize,
    work_between_critical_sections: usize,
    seconds_per_test: usize,
    test_iterations: usize,
) {
    let mut data = vec![];
    for _ in 0..test_iterations {
        let run_data = run_benchmark::<M>(
            num_threads,
            work_per_critical_section,
            work_between_critical_sections,
            seconds_per_test,
        );
        data.extend_from_slice(&run_data);
    }

    let average = data.iter().fold(0f64, |a, b| a + *b as f64) / data.len() as f64;
    let variance = data
        .iter()
        .fold(0f64, |a, b| a + ((*b as f64 - average).powi(2)))
        / data.len() as f64;
    data.sort();

    let k_hz = 1.0 / seconds_per_test as f64 / 1000.0;
    println!(
        "{:30} | {:10.3} kHz | {:10.3} kHz | {:10.3} kHz",
        M::name(),
        average * k_hz,
        data[data.len() / 2] as f64 * k_hz,
        variance.sqrt() * k_hz
    );
}

fn run_all(
    args: &[ArgRange],
    first: &mut bool,
    num_threads: usize,
    work_per_critical_section: usize,
    work_between_critical_sections: usize,
    seconds_per_test: usize,
    test_iterations: usize,
) {
    if num_threads == 0 {
        return;
    }
    if *first || !args[0].is_single() {
        println!("- Running with {} threads", num_threads);
    }
    if *first || !args[1].is_single() || !args[2].is_single() {
        println!(
            "- {} iterations inside lock, {} iterations outside lock",
            work_per_critical_section, work_between_critical_sections
        );
    }
    if *first || !args[3].is_single() {
        println!("- {} seconds per test", seconds_per_test);
    }
    *first = false;

    println!(
        "{:^30} | {:^14} | {:^14} | {:^14}",
        "name", "average", "median", "std.dev."
    );

    run_benchmark_iterations::<lock_bench::throughput::Mutex<f64>>(
        num_threads,
        work_per_critical_section,
        work_between_critical_sections,
        seconds_per_test,
        test_iterations,
    );

    run_benchmark_iterations::<lock_bench::eventually_fair::Mutex<f64>>(
        num_threads,
        work_per_critical_section,
        work_between_critical_sections,
        seconds_per_test,
        test_iterations,
    );

    run_benchmark_iterations::<parking_lot::Mutex<f64>>(
        num_threads,
        work_per_critical_section,
        work_between_critical_sections,
        seconds_per_test,
        test_iterations,
    );

    run_benchmark_iterations::<std::sync::Mutex<f64>>(
        num_threads,
        work_per_critical_section,
        work_between_critical_sections,
        seconds_per_test,
        test_iterations,
    );

    run_benchmark_iterations::<SystemMutex<f64>>(
        num_threads,
        work_per_critical_section,
        work_between_critical_sections,
        seconds_per_test,
        test_iterations,
    );

    run_benchmark_iterations::<SpinLock<f64>>(
        num_threads,
        work_per_critical_section,
        work_between_critical_sections,
        seconds_per_test,
        test_iterations,
    );
}

fn main() {
    let args = args::parse(&[
        "numThreads",
        "workPerCriticalSection",
        "workBetweenCriticalSections",
        "secondsPerTest",
        "testIterations",
    ]);
    let mut first = true;
    for num_threads in args[0] {
        for work_per_critical_section in args[1] {
            for work_between_critical_sections in args[2] {
                for seconds_per_test in args[3] {
                    for test_iterations in args[4] {
                        run_all(
                            &args,
                            &mut first,
                            num_threads,
                            work_per_critical_section,
                            work_between_critical_sections,
                            seconds_per_test,
                            test_iterations,
                        );
                    }
                }
            }
        }
    }
}
