# `std::collections::ordered_vec`

Status: experimental

Sorted-on-insert Vec<i64> with binary-search lookups.

## Public items

| Name | Kind | Description |
|---|---|---|
| `insert` | fn | Insert at the unique sorted position. |
| `remove_at` | fn | Remove the element at index `i`. |
| `contains` | fn | Return true iff `value` is present. |
| `index_of` | fn | Return the index of `value`, or -1. |
| `peek_min` | fn | Smallest element, or 0. |
| `peek_max` | fn | Largest element, or 0. |
| `len` | fn | Element count. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_ordered.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`contains`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_ordered.rs) | `fn contains(xs: Vec<i64>, value: i64) -> bool` | Return true iff `value` is present. |
| [`index_of`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_ordered.rs) | `fn index_of(xs: Vec<i64>, value: i64) -> Option<i64>` | Return the index of `value`, or -1. |
| [`insert`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_ordered.rs) | `fn insert(xs: Vec<i64>, value: i64) -> Vec<i64>` | Insert at the unique sorted position. |
| [`len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_ordered.rs) | `fn len(xs: Vec<i64>) -> i64` | Element count. |
| [`peek_max`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_ordered.rs) | `fn peek_max(xs: Vec<i64>) -> i64` | Largest element, or 0. |
| [`peek_min`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_ordered.rs) | `fn peek_min(xs: Vec<i64>) -> i64` | Smallest element, or 0. |
| [`remove_at`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/container_ordered.rs) | `fn remove_at(xs: Vec<i64>, index: i64) -> Vec<i64>` | Remove the element at index `i`. |
