# `std::runtime`

Status: experimental

Goroutine / scheduler introspection and tuning.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/runtime.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`arena_pop`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/runtime.rs) | `fn arena_pop() -> ()` | Closes the innermost arena region, freeing its slabs. |
| [`arena_push`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/runtime.rs) | `fn arena_push() -> ()` | Opens an arena region for bump allocation. |
| [`collect_cycles`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/runtime.rs) | `fn collect_cycles() -> ()` | Requests collection of unreachable reference cycles; returns `()`. |
| [`scheduler_stats_json`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/runtime.rs) | `fn scheduler_stats_json() -> String` | Returns a compact JSON snapshot of goroutine scheduler counters. |
| [`set_panic_hook`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/runtime.rs) | `fn set_panic_hook(hook: Fn(String) -> ()) -> ()` | Installs a hook invoked with the message on panic. |


## Cycle collection

`collect_cycles()` is experimental. The compiled runtime collects
thread-local RC graphs; values that have crossed a goroutine boundary are
excluded because concurrent mutation cannot safely participate in its
thread-local trial-deletion pass. Break such cycles with `Weak<T>`.

The bytecode VM uses `Arc`-backed values and currently treats this call as a
no-op. `Weak<T>::upgrade()` remains valid for the supported VM heap values,
but collection-driven weak invalidation is not a cross-tier guarantee yet.
