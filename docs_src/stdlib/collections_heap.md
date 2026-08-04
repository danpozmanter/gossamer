# `std::collections::heap`

Status: experimental

Binary min-heap (priority queue) over Vec<i64>. Re-bind shape: `let h = heap::push(h, v)`.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_heap.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_heap.rs) | `fn len(xs: Vec<i64>) -> i64` | Element count. |
| [`peek`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_heap.rs) | `fn peek(xs: Vec<i64>) -> Option<i64>` | Smallest element of the heap, if present. |
| [`pop`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_heap.rs) | `fn pop(xs: Vec<i64>) -> Vec<i64>` | Drop the root from the heap; returns the new heap (use `peek` first to read the value). |
| [`push`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_heap.rs) | `fn push(xs: Vec<i64>, value: i64) -> Vec<i64>` | Push an i64 onto the min-heap; returns the new heap. |
