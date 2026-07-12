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

