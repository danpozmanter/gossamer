# `std::iter`

Status: shipped

Sequence adapters over Vec<T>: map, filter, fold, zip, enumerate, chain, etc.

## Public items

| Name | Kind | Description |
|---|---|---|
| `count` | fn | Number of elements. |
| `take` | fn | First n elements. |
| `skip` | fn | All elements after the first n. |
| `zip` | fn | Pairs elements from two sequences. |
| `enumerate` | fn | Pairs each element with its index. |
| `chain` | fn | Concatenates two sequences. |
| `flatten` | fn | Flattens a Vec<Vec<T>> into Vec<T>. |
| `reversed` | fn | Returns a reversed copy. |
| `dedup` | fn | Removes consecutive duplicate elements. |
| `map` | fn | Applies f to each element, returning a new Vec. |
| `filter` | fn | Returns elements where f is true. |
| `fold` | fn | Reduces a sequence with an accumulator. |
| `flat_map` | fn | Maps f and flattens one level. |
| `any` | fn | True if any element satisfies f. |
| `all` | fn | True if every element satisfies f. |
| `sum` | fn | Sum of i64 or f64 elements. |

