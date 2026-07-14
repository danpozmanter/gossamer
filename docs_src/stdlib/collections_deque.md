# `std::collections::deque`

Status: shipped

Double-ended queue over Vec<i64>. Re-bind shape on every mutator.

## Public items

| Name | Kind | Description |
|---|---|---|
| `push_back` | fn | Append to the back. |
| `push_front` | fn | Prepend to the front. |
| `pop_back` | fn | Drop the back. |
| `pop_front` | fn | Drop the front. |
| `peek_front` | fn | Front element, or 0 if empty. |
| `peek_back` | fn | Back element, or 0 if empty. |
| `len` | fn | Element count. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_seq.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_seq.rs) | `fn len(xs: Deque<i64>) -> i64` | Element count. |
| [`peek_back`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_seq.rs) | `fn peek_back(xs: Deque<i64>) -> i64` | Back element, or 0 if empty. |
| [`peek_front`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_seq.rs) | `fn peek_front(xs: Deque<i64>) -> i64` | Front element, or 0 if empty. |
| [`pop_back`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_seq.rs) | `fn pop_back(xs: Deque<i64>) -> Deque<i64>` | Drop the back. |
| [`pop_front`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_seq.rs) | `fn pop_front(xs: Deque<i64>) -> Deque<i64>` | Drop the front. |
| [`push_back`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_seq.rs) | `fn push_back(xs: Deque<i64>, value: i64) -> Deque<i64>` | Append to the back. |
| [`push_front`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_seq.rs) | `fn push_front(xs: Deque<i64>, value: i64) -> Deque<i64>` | Prepend to the front. |
