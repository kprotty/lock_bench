usync benchmarks
====

This package contains a collection of benchmarks comparing
the synchronization primitives of `usync` to others in the rust ecosystem
such as `parking_lot`, `std`, and more. 

It is used more as a regression test but also serves to enable
understanding practical scenarios by supporting configurable
benchmarking options.

## Running a benchmark
Here is an example for running the blocking (sync) mutex benchmark.
Note that not providing any arguments (after the `--`) will display the usage/help page for the benchmark

```
Usage: cargo bench --bench sync_mutex -- [measure] [threads] [locked] [unlocked]
where:

 [measure]: [csv-ranged:time]   \\ List of time spent measuring for each mutex benchmark
 [threads]: [csv-ranged:count]  \\ List of thread counts for each benchmark
 [locked]: [csv-ranged:time]    \\ List of time spent inside the lock for each benchmark
 [unlocked]: [csv-ranged:time]  \\ List of time spent outside the lock for each benchmark

 [count]: {usize}
 [time]: {u128}[time_unit]
 [time_unit]: "ns" | "us" | "ms" | "s"

 [csv_ranged:{rule}]: {rule}
   | {rule} "-" {rule}                                  \\ randomized value in range
   | [csv_ranged:{rule}] "," [csv_ranged:{rule}]        \\ multiple permutations
```

`cargo bench --bench sync_mutex -- 1s 1-4,8,16,32 0ns,5ns-50ns 0ns-20us`