# `std::runtime`

Status: experimental

Goroutine / scheduler introspection and tuning.

## Public items

| Name | Kind | Description |
|---|---|---|
| `collect_cycles` | fn | Requests collection of unreachable reference cycles; returns `()`. |
| `scheduler_stats_json` | fn | Returns a compact JSON snapshot of goroutine scheduler counters. |
| `arena_push` | fn | Opens an arena region for bump allocation. |
| `arena_pop` | fn | Closes the innermost arena region, freeing its slabs. |
| `set_panic_hook` | fn | Installs a hook invoked with the message on panic. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## Cycle collection

`collect_cycles()` is experimental. The compiled runtime collects
thread-local RC graphs; values that have crossed a goroutine boundary are
excluded because concurrent mutation cannot safely participate in its
thread-local trial-deletion pass. Break such cycles with `Weak<T>`.

The bytecode VM uses `Arc`-backed values and currently treats this call as a
no-op. `Weak<T>::upgrade()` remains valid for the supported VM heap values,
but collection-driven weak invalidation is not a cross-tier guarantee yet.
