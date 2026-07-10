# `std::runtime`

Status: shipped

Goroutine / scheduler introspection and tuning.

## Public items

| Name | Kind | Description |
|---|---|---|
| `collect_cycles` | fn | Runs the reference-cycle collector and returns objects reclaimed. |
| `scheduler_stats_json` | fn | Returns a compact JSON snapshot of goroutine scheduler counters. |
| `arena_push` | fn | Opens an arena region for bump allocation. |
| `arena_pop` | fn | Closes the innermost arena region, freeing its slabs. |
| `set_panic_hook` | fn | Installs a hook invoked with the message on panic. |

