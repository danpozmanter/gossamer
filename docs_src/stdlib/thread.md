# `std::thread`

Status: shipped

Native OS threads. For goroutines use the `go expr` syntax.

## Public items

| Name | Kind | Description |
|---|---|---|
| `JoinHandle` | type | Owned handle to a spawned OS thread. |
| `yield_now` | fn | Hints to the scheduler to switch to another runnable thread. |
| `num_cpus` | fn | Returns the number of logical CPUs available. |

