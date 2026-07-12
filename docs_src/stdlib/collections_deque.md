# `std::collections::deque`

Status: experimental

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

