# `std::collections::stack`

Status: experimental

LIFO stack over Vec<i64>. Re-bind shape: `let s = stack::push(s, v)`.

## Public items

| Name | Kind | Description |
|---|---|---|
| `push` | fn | Push an i64 onto the top; returns the new stack. |
| `pop` | fn | Drop the top; returns the new stack. |
| `peek` | fn | Top element, or 0 if empty. |
| `len` | fn | Element count. |

