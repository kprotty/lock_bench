# mutex experimentations
Benchmarking a variety of mutex implements to see under which conditions they perform best in their respected properties.

## benchmarking
the benches folder contains the mutex benchmark found in [parking_lot](https://github.com/Amanieu/parking_lot) with the ability to add more entries. To run the benchmark, execute:

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
