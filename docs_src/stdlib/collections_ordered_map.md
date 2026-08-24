# `std::collections::ordered_map`

Status: experimental

Sorted key/value map (i64 -> i64) backed by a flat pair Vec. Re-bind on every mutator.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/container_set_map.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`contains_key`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/container_set_map.rs) | `fn contains_key(map: OrderedMap<String, i64>, key: String) -> bool` | Key-membership test. |
| [`get`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/container_set_map.rs) | `fn get(map: OrderedMap<String, i64>, key: String) -> Option<i64>` | Lookup; returns 0 if absent. |
| [`insert`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/container_set_map.rs) | `fn insert(map: OrderedMap<String, i64>, key: String, value: i64) -> OrderedMap<String, i64>` | Set key => value. |
| [`len`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/container_set_map.rs) | `fn len(map: OrderedMap<String, i64>) -> i64` | Pair count. |
| [`remove`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/container_set_map.rs) | `fn remove(map: OrderedMap<String, i64>, key: String) -> OrderedMap<String, i64>` | Remove a key. |
