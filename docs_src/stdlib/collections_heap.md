# `std::collections::heap`

Status: experimental

Binary min-heap (priority queue) over Vec<i64>. Re-bind shape: `let h = heap::push(h, v)`.

## Public items

| Name | Kind | Description |
|---|---|---|
| `push` | fn | Push an i64 onto the min-heap; returns the new heap. |
| `pop` | fn | Drop the root from the heap; returns the new heap (use `peek` first to read the value). |
| `peek` | fn | Smallest element of the heap, or 0 if empty. |
| `len` | fn | Element count. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_heap.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_heap.rs) | `fn len(xs: Heap<i64>) -> i64` | Element count. |
| [`peek`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_heap.rs) | `fn peek(xs: Heap<i64>) -> i64` | Smallest element of the heap, or 0 if empty. |
| [`pop`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_heap.rs) | `fn pop(xs: Heap<i64>) -> Heap<i64>` | Drop the root from the heap; returns the new heap (use `peek` first to read the value). |
| [`push`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_heap.rs) | `fn push(xs: Heap<i64>, value: i64) -> Heap<i64>` | Push an i64 onto the min-heap; returns the new heap. |
