# `std::context`

Status: experimental

Request-scoped cancellation, deadlines, and timeouts.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/context.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Context`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/context.rs) | `type Context` | Cancellation-aware context handle. |
