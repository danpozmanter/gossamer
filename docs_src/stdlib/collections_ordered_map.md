# `std::collections::ordered_map`

Sorted key/value map (i64 -> i64) backed by a flat pair Vec. Re-bind on every mutator.

## Public items

| Name | Kind | Description |
|---|---|---|
| `insert` | fn | Set key => value. |
| `remove` | fn | Remove a key. |
| `get` | fn | Lookup; returns 0 if absent. |
| `contains_key` | fn | Key-membership test. |
| `len` | fn | Pair count. |

