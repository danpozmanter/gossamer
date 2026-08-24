# `std::collections::queue`

Status: experimental

FIFO queue over Vec<i64>. Re-bind shape: `let q = queue::push(q, v)`.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/container_seq.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`len`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/container_seq.rs) | `fn len(xs: Vec<i64>) -> i64` | Element count. |
| [`peek`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/container_seq.rs) | `fn peek(xs: Vec<i64>) -> Option<i64>` | Front element, if present. |
| [`pop`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/container_seq.rs) | `fn pop(xs: Vec<i64>) -> Vec<i64>` | Drop the front element; returns the new queue. |
| [`push`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/container_seq.rs) | `fn push(xs: Vec<i64>, value: i64) -> Vec<i64>` | Append an i64 to the back; returns the new queue. |
