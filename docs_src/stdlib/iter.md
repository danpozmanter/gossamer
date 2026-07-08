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
| `rev` | fn | Returns a rev copy. |
| `dedup` | fn | Removes consecutive duplicate elements. |
| `map` | fn | Applies f to each element, returning a new Vec. |
| `filter` | fn | Returns elements where f is true. |
| `fold` | fn | Reduces a sequence with an accumulator. |
| `flat_map` | fn | Maps f and flattens one level. |
| `any` | fn | True if any element satisfies f. |
| `all` | fn | True if every element satisfies f. |
| `sum` | fn | Sum of i64 or f64 elements. |
| `product` | fn | Product of i64 or f64 elements. |
| `min` | fn | Smallest element, or None when empty. |
| `max` | fn | Largest element, or None when empty. |
| `range` | fn | Half-open integer sequence [start, end). |
| `range_inclusive` | fn | Closed integer sequence [start, end]. |
| `repeat` | fn | A value repeated n times. |
| `unzip` | fn | Splits a sequence of pairs into two Vecs. |
| `windows` | fn | Overlapping windows of width n. |
| `pairwise` | fn | Consecutive overlapping pairs. |
| `chunks` | fn | Non-overlapping chunks of length n. |
| `for_each` | fn | Applies f to each element for its side effect. |
| `filter_map` | fn | Maps each element and keeps the Some results. |
| `reduce` | fn | Folds with the first element as the initial accumulator. |
| `scan` | fn | Folds while yielding each intermediate accumulator. |
| `sum_by` | fn | Sum of f(element) over the sequence. |
| `product_by` | fn | Product of f(element) over the sequence. |
| `find` | fn | First element satisfying f, or None. |
| `position` | fn | Index of the first element satisfying f, or None. |
| `find_map` | fn | First Some result of f over the sequence. |
| `take_while` | fn | Leading run of elements satisfying f. |
| `skip_while` | fn | Elements after the leading run satisfying f. |
| `partition` | fn | Splits into (matching, non-matching) by f. |
| `sort_by` | fn | Sorted copy ordered by the comparison closure. |
| `sort_by_key` | fn | Sorted copy ordered by a derived key. |
| `min_by` | fn | Smallest element by the comparison closure. |
| `max_by` | fn | Largest element by the comparison closure. |
| `min_by_key` | fn | Element with the smallest derived key. |
| `max_by_key` | fn | Element with the largest derived key. |
| `chunk_by` | fn | Groups elements into a map keyed by f. |
| `count_by` | fn | Counts elements per key derived by f. |

