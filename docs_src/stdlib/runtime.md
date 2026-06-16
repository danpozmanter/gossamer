# `std::runtime`

Status: shipped

Goroutine / scheduler introspection and tuning.

## Public items

| Name | Kind | Description |
|---|---|---|
| `max_procs` | fn | Returns the current goroutine concurrency cap. |
| `set_max_procs` | fn | Sets the goroutine concurrency cap (GOMAXPROCS-equivalent). |
| `num_cpus` | fn | Logical CPU cores visible to the process. |
| `collect_cycles` | fn | Runs the reference-cycle collector and returns objects reclaimed. |
| `arena_push` | fn | Opens an arena region for bump allocation. |
| `arena_pop` | fn | Closes the innermost arena region, freeing its slabs. |
| `set_panic_hook` | fn | Installs a hook invoked with the message on panic. |

