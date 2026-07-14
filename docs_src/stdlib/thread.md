# `std::thread`

Status: shipped

OS-thread scheduling hints and CPU introspection; user concurrency uses goroutines, not thread spawning.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/thread.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`num_cpus`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/thread.rs) | `fn num_cpus() -> i64` | Returns the number of logical CPUs available. |
| [`yield_now`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/thread.rs) | `fn yield_now() -> ()` | Hints to the scheduler to switch to another runnable thread. |
