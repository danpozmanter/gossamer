# `std::collections::queue`

Status: experimental

FIFO queue over Vec<i64>. Re-bind shape: `let q = queue::push(q, v)`.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_seq.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_seq.rs) | `fn len(xs: Queue<i64>) -> i64` | Element count. |
| [`peek`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_seq.rs) | `fn peek(xs: Queue<i64>) -> i64` | Front element, or 0 if empty. |
| [`pop`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_seq.rs) | `fn pop(xs: Queue<i64>) -> Queue<i64>` | Drop the front element; returns the new queue. |
| [`push`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_seq.rs) | `fn push(xs: Queue<i64>, value: i64) -> Queue<i64>` | Append an i64 to the back; returns the new queue. |
