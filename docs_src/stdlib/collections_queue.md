# `std::collections::queue`

Status: shipped

FIFO queue over Vec<i64>. Re-bind shape: `let q = queue::push(q, v)`.

## Public items

| Name | Kind | Description |
|---|---|---|
| `push` | fn | Append an i64 to the back; returns the new queue. |
| `pop` | fn | Drop the front element; returns the new queue. |
| `peek` | fn | Front element, or 0 if empty. |
| `len` | fn | Element count. |

