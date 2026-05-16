# `std::collections::ordered_vec`

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

