use std::{
    fmt,
    ops::Div,
    str::Chars,
    iter::Peekable,
    cell::UnsafeCell,
    mem::MaybeUninit,
    convert::TryInto,
    time::{Duration, Instant},
    sync::{Arc, Barrier, atomic::{AtomicBool, Ordering}},
};

#[cfg(any(windows, unix))]
#[path = "./mutexes/os.rs"]
mod _os;

#[path = "./mutexes/spin.rs"] mod _spin;
#[path = "./mutexes/std.rs"] mod _std;
#[path = "./mutexes/parking_lot.rs"] mod _parking_lot;

pub fn main() {
    let work_per_ns = WorkUnit::work_per_ns();
    let parsed = Parser::parse().unwrap();

    for ctx in parsed.collect() {
        ctx.with_benchmarker(work_per_ns, |b| {
            #[cfg(any(windows, unix))] b.bench::<_os::Lock>();
            b.bench::<_spin::Lock>();
            b.bench::<_std::Lock>();
            b.bench::<_parking_lot::Lock>();
        });
    }

    println!();
}

pub trait Lock: Sync + Sized {
    fn name() -> &'static str;

    fn new() -> Self;

    fn with<F: FnOnce()>(&self, f: F);
}

#[derive(Debug, Copy, Clone)]
enum ParseUnit {
    Nanos,
    Micros,
    Millis,
    Secs,
}

#[derive(Debug, Copy, Clone)]
struct ParseValue {
    value: u128,
    unit: Option<ParseUnit>,
}

impl Into<usize> for ParseValue {
    fn into(self) -> usize {
        if self.unit.is_some() {
            unreachable!("expected usize, found time value")
        } else if let Ok(value) = self.value.try_into() {
            value
        } else {
            unreachable!("invalid usize value")
        }
    }
}

impl Into<Duration> for ParseValue {
    fn into(self) -> Duration {
        match self.unit {
            Some(ParseUnit::Nanos) => Duration::from_nanos(self.value.try_into().unwrap()),
            Some(ParseUnit::Micros) => Duration::from_micros(self.value.try_into().unwrap()),
            Some(ParseUnit::Millis) => Duration::from_millis(self.value.try_into().unwrap()),
            Some(ParseUnit::Secs) => Duration::from_secs(self.value.try_into().unwrap()),
            _ => unreachable!("invalid time value"),
        }
    }
}

#[derive(Debug)]
enum ParseItem {
    Range(ParseValue, ParseValue),
    Value(ParseValue),
}

#[derive(Default, Debug)]
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

        for threads in threads {
            for &locked in locked.iter() {
                for &unlocked in unlocked.iter() {
                    for &measure in measure.iter() {
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
        println!(" [measure]: [csv-ranged:time]\t\\\\ List of time spent measuring for each mutex benchmark");
        println!(" [threads]: [csv-ranged:count]\t\\\\ List of thread counts for each benchmark");
        println!(" [locked]: [csv-ranged:time]\t\\\\ List of time spent inside the lock for each benchmark");
        println!(" [unlocked]: [csv-ranged:time]\t\\\\ List of time spent outside the lock for each benchmark");
        
        println!();
        println!(" [count]: {{usize}}");
        println!(" [time]: {{u128}}[time_unit]");
        println!(" [time_unit]: \"ns\" | \"us\" | \"ms\" | \"s\"");
       
        println!();
        println!(" [csv_ranged:{{rule}}]: {{rule}}");
        println!("   | {{rule}} \"-\" {{rule}} \t\t\t\t\t\\\\ randomized value in range");
        println!("   | [csv_ranged:{{rule}}] \",\" [csv_ranged:{{rule}}] \t\\\\ multiple permutations");
        println!();
    }
    
    fn error(message: &'static str) -> ! {
        eprintln!("Error: {:?}\n", message);
        Self::print_help(std::env::args().next().unwrap());
        std::process::exit(1)
    }

    fn parse_value(chars: &mut Peekable<Chars<'_>>) -> ParseValue {
        let mut value = None;
        while let Some(&chr) = chars.peek() {
            if let Some(digit) = chr.to_digit(10) {
                let _ = chars.next();
                let d = digit as u128;
                value = Some(value.map(|v| (v * 10) + d).unwrap_or(d));

            } else {
                break;
            }
        }

        ParseValue {
            value: value.unwrap_or_else(|| Self::error("invalid argument value")),
            unit: match chars.peek().map(|c| *c) {
                Some(d) if matches!(d, 'n'|'u'|'m'|'s') => {
                    if chars.next().unwrap() == 's' {
                        Some(ParseUnit::Secs)
                    } else if chars.next() == Some('s') {
                        Some(match d {
                            'n' => ParseUnit::Nanos,
                            'u' => ParseUnit::Micros,
                            'm' => ParseUnit::Millis,
                            _ => unreachable!(),
                        })
                    } else {
                        Self::error("invalid timer value")
                    }
                },
                _ => None,
            },
        }
    }

    fn parse_arg<T>(
        results: &mut Vec<T>,
        input: Option<String>,
        gen_result: impl Fn(&mut Vec<T>, ParseItem),
    ) {
        let input = input.unwrap_or_else(|| Self::error("invalid argument"));
        let mut chars = input.chars().peekable();
        loop {
            let start = Self::parse_value(&mut chars);
            let mut end = None;
            
            if let Some(&'-') = chars.peek() {
                let _ = chars.next();
                end = Some(Self::parse_value(&mut chars));
            }

            gen_result(results, match end {
                Some(end) => ParseItem::Range(start, end),
                None => ParseItem::Value(start),
            });

            match chars.next() {
                None => break,
                Some(',') => continue,
                Some(_) => unreachable!("invalid argument continuation"),
            }
        }
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
                
            Self::parse_arg(&mut parsed.measure, args.next(), |results, item| match item {
                ParseItem::Range(_, _) => unreachable!("measure time does not support ranges"),
                ParseItem::Value(value) => results.push(value.into()), 
            });

            Self::parse_arg(&mut parsed.threads, args.next(), |results, item| {
                let (start, end): (usize, usize) = match item {
                    ParseItem::Range(start, end) => (start.into(), end.into()),
                    ParseItem::Value(value) => {
                        let num_threads = value.into();
                        (num_threads, num_threads)
                    },
                };
                (start .. (end + 1)).for_each(|thread| results.push(thread));
            });

            fn read_work_unit(results: &mut Vec<WorkUnit>, item: ParseItem) {
                let (from, to) = match item {
                    ParseItem::Range(start, end) => (start.into(), Some(end.into())),
                    ParseItem::Value(value) => (value.into(), None),
                };
                if let Some(to) = to {
                    if from >= to {
                        Parser::error("invalid time range value");
                    }
                }
                results.push(WorkUnit { from, to })
            }

            Self::parse_arg(&mut parsed.locked, args.next(), read_work_unit);
            Self::parse_arg(&mut parsed.unlocked, args.next(), read_work_unit);
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
        
        println!();
        println!(
            "measure={:?} threads={:?} locked={:?} unlocked={:?}",
            self.measure_time,
            self.num_threads,
            self.work_inside,
            self.work_outside,
        );

        println!("------------------------------------------------------------");
        println!("{:?}", BenchmarkResult {
            name: "name".to_string(),
            mean: "average".to_string(),
            median: "median".to_string(),
            stdev: "std. dev.".to_string(),
        });

        f(&benchmarker);
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
    fn bench<L: Lock + Send + Sync + 'static>(&self) {
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
                    let mut iterations = 0usize;
                    let mut prng = {
                        let stack = 0usize;
                        let ptr = &stack as *const _ as usize;
                        ptr.wrapping_mul(31) as u64
                    };

                    run_context.0.wait();
                    while run_context.1.load(Ordering::SeqCst) {
                        let inside = work_inside.count(&mut prng);
                        mutex.lock.with(|| WorkUnit::run(inside));
                        iterations += 1;

                        if !run_context.1.load(Ordering::SeqCst) {
                            break;
                        } else {
                            let outside = work_outside.count(&mut prng);
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
            .map(|t| t.join().unwrap())
            .collect::<Vec<_>>();

        let mean = results
            .iter()
            .fold(0f64, |mean, &iters| mean + (iters as f64))
            .div(results.len() as f64);

        let mut stdev = results
            .iter()
            .fold(0f64, |stdev, &iters| {
                let r = (iters as f64) - mean;
                stdev + (r * r)
            });

        if results.len() > 1 {
            stdev /= (results.len() - 1) as f64;
            stdev = stdev.sqrt();
        }

        println!("{:?}", BenchmarkResult {
            name: L::name().to_string(),
            mean: mean.floor().to_string(),
            median: results[results.len() / 2].to_string(),
            stdev: stdev.floor().to_string(),
        });
    }
}

struct BenchmarkResult {
    name: String,
    mean: String,
    median: String,
    stdev: String,
}

impl fmt::Debug for BenchmarkResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:<18} | {:>12} | {:>11} | {:>10}",
            self.name,
            self.mean,
            self.median,
            self.stdev,
        )
    }
}

#[derive(Copy, Clone)]
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
        match self.to {
            Some(to) => write!(f, "rand({:?}, {:?})", self.from, to),
            None => write!(f, "{:?}", self.from),
        }
    }
}