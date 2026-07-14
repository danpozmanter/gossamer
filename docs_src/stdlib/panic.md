# `std::panic`

Status: experimental

Panic / `catch_unwind` integration.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/panic.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`panic`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/panic.rs) | `macro panic!(...)` | Aborts the current goroutine with a message. |
