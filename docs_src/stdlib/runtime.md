# `std::runtime`

Status: shipped

Goroutine / scheduler introspection and tuning.

## Public items

| Name | Kind | Description |
|---|---|---|
| `max_procs` | fn | Returns the current goroutine concurrency cap. |
| `set_max_procs` | fn | Sets the goroutine concurrency cap (GOMAXPROCS-equivalent). |
| `num_cpus` | fn | Logical CPU cores visible to the process. |

