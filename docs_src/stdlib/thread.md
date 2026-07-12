# `std::thread`

Status: experimental

OS-thread scheduling hints and CPU introspection; user concurrency uses goroutines, not thread spawning.

## Public items

| Name | Kind | Description |
|---|---|---|
| `yield_now` | fn | Hints to the scheduler to switch to another runnable thread. |
| `num_cpus` | fn | Returns the number of logical CPUs available. |

