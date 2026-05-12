# `std::thread`

Native OS threads. For goroutines use the `go expr` syntax.

## Public items

| Name | Kind | Description |
|---|---|---|
| `spawn` | fn | Spawns a new OS thread; returns a JoinHandle. |
| `JoinHandle` | type | Owned handle to a spawned OS thread. |
| `join` | fn | Waits for the thread to finish; returns its result. |
| `yield_now` | fn | Hints to the scheduler to switch to another runnable thread. |
| `num_cpus` | fn | Returns the number of logical CPUs available. |

