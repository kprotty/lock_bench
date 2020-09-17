# mutex experimentations
Implemented an eventually fair mutex as well as a throughput optimized, both only the size of a machine word.

## benchmarking
the examples folder contains the mutex benchmark found in [parking_lot](https://github.com/Amanieu/parking_lot) with more entries such as a cross platform systems lock, a spin lock, and the two usize mutexes above. To run the benchmark, execute:

```
Usage: cargo bench -- [measure] [threads] [locked] [unlocked]
where:

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

example: `cargo bench -- 1s 2,4-6,8,16 50ns 0ns-1us`
