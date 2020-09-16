use std::{
    fmt,
    ops::Div,
    cell::UnsafeCell,
    mem::MaybeUninit,
    convert::TryInto,
    time::{Duration, Instant},
    sync::{Arc, Barrier, atomic::{AtomicBool, Ordering}},
};

#[path = "./mutexes/spin.rs"]
mod spin;

pub fn main() {
    let work_per_ns = WorkUnit::work_per_ns();
    let parsed = Parser::parse().unwrap();

    for ctx in parsed.collect() {
        ctx.with_benchmarker(work_per_ns, |b| {
            b.bench::<spin::SpinLock>();
        });
    }
}

pub trait Lock: Sync + Sized {
    fn name() -> &'static str;

    fn new() -> Self;

    fn with<F: FnOnce()>(&self, f: F);
}

#[derive(Default)]
struct Parser {
    threads: Vec<usize>,
    locked: Vec<WorkUnit>,
    unlocked: Vec<WorkUnit>,
    measure: Vec<Duration>,
}

impl Parser {
    fn collect(self) -> Vec<Context> {
        let Self { threads, locked, unlocked, measure } = self;
        let mut contexes = Vec::new();

        for threads in threads.into_iter() {
            for locked in locked.into_iter() {
                for unlocked in unlocked.into_iter() {
                    for measure in measure.into_iter() {
                        contexes.push(Context {
                            num_threads: threads,
                            work_inside: locked,
                            work_outside: unlocked,
                            measure_time: measure,
                        });
                    }
                }
            }
        }

        contexes
    }

    fn print_help(exe: String) {
        println!("Usage: {} [measure] [threads] [locked] [unlocked]", exe);
        println!("where:");

        println!();
        println!(" [measure]: [time]\t\\\\ Time spent measuring for each mutex benchmark");
        println!(" [threads]: [csv-ranged:count]\t\\\\ List of thread counts for each benchmark");
        println!(" [locked]: [csv-ranged:time]\t\\\\ List of time spent inside the lock for each benchmark");
        println!(" [unlocked]: [csv-ranged:time]\t\\\\ List of time spent outside the lock for each benchmark");
        
        println!();
        println!(" [count]: {{usize}}");
        println!(" [time]: {{u128}}[time_unit]");
        println!(" [time_unit]: \"ns\" | \"us\" | \"ms\" | \"s\"");
        println!(" ");

        println!(" [csv_ranged:{{rule}}]: {{rule}}");
        println!("   | {{rule}} \"-\" {{rule}} \t\\\\ randomized value in range");
        println!("   | [csv_ranged:{{rule}}] \",\" [csv_ranged:{{rule}}] \t\\\\ multiple permutations");
    }

    fn parse() -> Option<Self> {
        let mut parsed = Self::default();

        {
            let mut args = std::env::args().peekable();
            let exe = args.next().unwrap();
            if args.peek().is_none() {
                Self::print_help(exe);
                return None;
            }

            fn parse_num(input: &mut &str) -> u128 {
                let mut result: Option<u128> = None;
                while let Some(c) = (*input).chars().nth(0) {
                    if let Some(d) = c.to_digit(10) {
                        let d = d as u128;
                        result = Some(result.map(|r| (r * 10) + d).unwrap_or(d));
                        *input = &(*input)[1..];
                    } else {
                        break;
                    }
                }
                result.expect("invalid argument value")
            }

            let parse_usize = |input| -> usize {
                parse_num(input).try_into().unwrap()
            };

            let parse_time = |input| {
                let amount = parse_num(input);
                let (advance, duration) = match &(*input)[0..2] {
                    "s" => (1, Duration::from_secs(amount.try_into().unwrap())),
                    "ms" => (2, Duration::from_millis(amount.try_into().unwrap())),
                    "us" => (2, Duration::from_micros(amount.try_into().unwrap())),
                    "ns" => (2, Duration::from_nanos(amount.try_into().unwrap())),
                    _ => unreachable!("invalid time unit"),
                };
                *input = &(*input)[advance..];
                duration
            };

            fn parse_arg<'a, 'b, 'c: 'b, I, T>(
                items: &'a mut Vec<I>,
                arg: Option<String>,
                to_item: impl Fn(T, Option<T>) -> I,
                parse_item_type: impl Fn(&'b mut &'c str) -> T,
            ) {
                let arg = arg.expect("invalid argument");
                let mut input = arg.as_str();

                while input.len() > 0 {
                    let first = parse_item_type(&mut input);
                    let mut second = None;

                    if input.chars().nth(0) == Some('-') {
                        input = &input[1..];
                        second = Some(parse_item_type(&mut input));
                    }

                    items.push(to_item(first, second));
                    if input.chars().nth(0) == Some(',') {
                        input = &input[1..];
                    } else {
                        break;
                    }
                }
            }

            fn to_work_unit(from: Duration, to: Option<Duration>) -> WorkUnit {
                WorkUnit { from, to }
            }

            parse_arg(&mut parsed.measure, args.next(), |a, _| a, parse_time);
            parse_arg(&mut parsed.threads, args.next(), |a, _| a, parse_usize);
            parse_arg(&mut parsed.locked, args.next(), to_work_unit, parse_time);
            parse_arg(&mut parsed.unlocked, args.next(), to_work_unit, parse_time);
        }

        if parsed.measure.is_empty() {
            parsed.measure.push(Duration::from_secs(1));
        }

        if parsed.threads.is_empty() {
            parsed.threads.push(1);
        }

        if parsed.locked.is_empty() {
            parsed.locked.push(WorkUnit {
                from: Duration::new(0, 0),
                to: None,
            });
        }

        if parsed.unlocked.is_empty() {
            parsed.unlocked.push(WorkUnit {
                from: Duration::new(0, 0),
                to: None,
            });
        }

        Some(parsed)
    }
}

struct Context {
    num_threads: usize,
    work_inside: WorkUnit,
    work_outside: WorkUnit,
    measure_time: Duration,
}

impl Context {
    fn with_benchmarker(&self, work_per_ns: u128, f: impl FnOnce(&Benchmarker<'_>)) {
        let benchmarker = Benchmarker {
            ctx: self,
            work_per_ns,
        };

        println!(
            "threads={} locked={:?} unlocked={:?}",
            self.num_threads,
            self.work_inside,
            self.work_outside,
        );

        println!("{:?}", "-".repeat(20));
        println!("{:?}", BenchmarkResult {
            name: "lock name".to_string(),
            mean: "avg. locks/s".to_string(),
            stdev: "stdev. locks/thread".to_string(),
        });

        f(&benchmarker);
        println!();
    }
}

struct Benchmarker<'a> {
    ctx: &'a Context,
    work_per_ns: u128,
}

#[repr(C)]
struct Mutex<L: Lock> {
    _padding1: MaybeUninit<[u8; 300]>,
    lock: L,
    _padding2: MaybeUninit<[u8; 300]>,
}

unsafe impl<L: Lock> Send for Mutex<L> {}

impl<'a> Benchmarker<'a> {
    fn bench<L: Lock>(&self) {
        unsafe {
            let mutex = Arc::new(Mutex {
                _padding1: MaybeUninit::uninit(),
                lock: L::new(),
                _padding2: MaybeUninit::uninit(),
            });
            
            let num_threads = self.ctx.num_threads;
            let work_inside = self.ctx.work_inside.scaled(self.work_per_ns);
            let work_outside = self.ctx.work_outside.scaled(self.work_per_ns);
            let run_context = Arc::new((Barrier::new(num_threads + 1), AtomicBool::new(true)));

            let threads = (0..num_threads)
                .map(|_| {
                    let mutex = mutex.clone();
                    let run_context = run_context.clone();

                    std::thread::spawn(move || {
                        let mutex = mutex;
                        let mut iterations = 0usize;
                        let mut prng = {
                            let stack = 0usize;
                            let ptr = &stack as *const _ as usize;
                            ptr.wrapping_mul(31) as u64
                        };

                        run_context.0.wait();
                        while run_context.1.load(Ordering::SeqCst) {
                            let inside = work_inside.count(&mut prng);
                            let outside = work_outside.count(&mut prng);
                            
                            mutex.lock.with(|| WorkUnit::run(inside));
                            iterations += 1;

                            if !run_context.1.load(Ordering::SeqCst) {
                                break;
                            } else {
                                WorkUnit::run(outside);
                            }
                        }

                        iterations
                    })
                })
                .collect::<Vec<_>>();

            run_context.0.wait();
            std::thread::sleep(self.ctx.measure_time);
            run_context.1.store(false, Ordering::SeqCst);

            let results = threads
                .into_iter()
                .map(|t| t.join().unwrap());

            let mean = results
                .fold(0f64, |mean, iters| mean + (iters as f64))
                .div(results.len() as f64);

            let mut stdev = results.fold(0f64, |stdev, iters| {
                let r = (iters as f64) - mean;
                stdev + (r * r)
            });

            if results.len() > 1 {
                stdev /= (results.len() - 1) as f64;
                stdev = stdev.sqrt();
            }

            println!("{:?}", BenchmarkResult {
                name: L::name().to_string(),
                mean: mean.to_string(),
                stdev: stdev.to_string(),
            });
        }
    }
}

struct BenchmarkResult {
    name: String,
    mean: String,
    stdev: String,
}

impl fmt::Debug for BenchmarkResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:20} | {:12} | {:12}",
            self.name,
            self.mean,
            self.stdev,
        )
    }
}

struct WorkUnit {
    from: Duration,
    to: Option<Duration>,
}

impl WorkUnit {
    fn scaled(&self, div: u128) -> Self {
        let scale = |duration: Duration| {
            let nanos = duration.as_nanos() / div;
            Duration::from_nanos(nanos.try_into().unwrap())
        };

        Self {
            from: scale(self.from),
            to: self.to.map(scale),
        }
    }

    fn count(&self, prng: &mut u64) -> u128 {
        let mut xorshift64 = || {
            let mut x = *prng;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *prng = x;
            x
        };

        let min = self.from.as_nanos();
        match self.to {
            None => min,
            Some(to) => {
                let max = to.as_nanos();
                let high = xorshift64() as u128;
                let low = xorshift64() as u128;
                let rng = (high << 64) | low;
                (rng % (max - min + 1)) + min
            }
        }
    }

    fn run(iterations: u128) {
        for _ in 0..iterations {
            Self::work();
        }
    }

    fn work() {
        unsafe {
            let stack = UnsafeCell::new(0usize);
            let v = std::ptr::read_volatile(stack.get());
            std::ptr::write_volatile(stack.get(), v);
        }
    }

    fn work_per_ns() -> u128 {
        let compute_work_per_ns = || {
            fn record(op: impl FnOnce()) -> Duration {
                let start = Instant::now();
                op();
                Instant::now() - start
            }

            let timer_overhead = record(|| unsafe {
                let result = UnsafeCell::new(MaybeUninit::uninit());
                std::ptr::write_volatile(result.get(), MaybeUninit::new(Instant::now()));
            });

            let num_steps = 10_000;
            let ns = record(|| WorkUnit::run(num_steps));

            let elapsed = (ns - timer_overhead).as_nanos();
            (num_steps / elapsed).max(1)
        };

        let attempts = 10;
        (0..attempts)
            .map(|_| compute_work_per_ns())
            .fold(0, |avg, work_per_ns| avg + work_per_ns)
            .div(attempts)
    }
}

impl fmt::Debug for WorkUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.from)?;
        if let Some(to) = self.to {
            write!(f, "{:?}", to)?;
        }
        Ok(())
    }
}