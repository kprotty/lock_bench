# mutex experimentations
Implemented an eventually fair mutex as well as a throughput optimized, both only the size of a machine word.

## benchmarking
the examples folder contains the mutex benchmark found in [parking_lot](https://github.com/Amanieu/parking_lot) with more entries such as a cross platform systems lock, a spin lock, and the two usize mutexes above. To run the benchmark, execute:

```bash
cargo run --release --example mutex [threads] [work_inside_lock] [work_outside_lock] [seconds_per_mutex_iter] [iters_per_mutex]

# example 

cargo run --release --example mutex 1:4 10 100 1 1
```

As for the results, as far as I can discern:
* **average**: amount of locks per second. higher = better throughput
* **median**: throughput, but more resistant to skewed counts
* **std. dev**: variance between threads, lower = better latency
