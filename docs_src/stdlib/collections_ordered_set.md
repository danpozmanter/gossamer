# `std::collections::ordered_set`

Status: experimental

Sorted set of i64 with binary-search lookups. Re-bind shape on every mutator.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/container_set_map.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`contains`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/container_set_map.rs) | `fn contains(xs: OrderedSet<i64>, value: i64) -> bool` | Membership test. |
| [`insert`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/container_set_map.rs) | `fn insert(xs: OrderedSet<i64>, value: i64) -> OrderedSet<i64>` | Insert (sorted, no duplicates). |
| [`len`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/container_set_map.rs) | `fn len(xs: OrderedSet<i64>) -> i64` | Element count. |
| [`remove`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/container_set_map.rs) | `fn remove(xs: OrderedSet<i64>, value: i64) -> OrderedSet<i64>` | Remove a value. |
