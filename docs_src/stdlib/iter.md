# `std::iter`

Status: experimental

Sequence adapters over Vec<T>: map, filter, fold, zip, enumerate, chain, etc.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`all`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn all<T>(predicate: Fn(T) -> bool, items: Vec<T>) -> bool` | True if every element satisfies f. |
| [`any`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn any<T>(predicate: Fn(T) -> bool, items: Vec<T>) -> bool` | True if any element satisfies f. |
| [`chain`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn chain<T>(left: Vec<T>, right: Vec<T>) -> Vec<T>` | Concatenates two sequences. |
| [`chunk_by`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn chunk_by<T, K: Eq>(key: Fn(T) -> K, items: Vec<T>) -> HashMap<K, Vec<T>>` | Groups elements into a map keyed by f. |
| [`chunks`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn chunks<T>(n: i64, items: Vec<T>) -> Vec<Vec<T>>` | Non-overlapping chunks of length n. |
| [`collect`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn collect<T>(items: Vec<T>) -> Vec<T>` | Materializes a sequence into a Vec. |
| [`count`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn count<T>(items: Vec<T>) -> i64` | Number of elements. |
| [`count_by`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn count_by<T, K: Eq>(key: Fn(T) -> K, items: Vec<T>) -> HashMap<K, i64>` | Counts elements per key derived by f. |
| [`dedup`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn dedup<T: Eq>(items: Vec<T>) -> Vec<T>` | Removes consecutive duplicate elements. |
| [`empty`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn empty<T>() -> Vec<T>` | Empty Vec. |
| [`enumerate`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn enumerate<T>(items: Vec<T>) -> Vec<(i64, T)>` | Pairs each element with its index. |
| [`filter`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn filter<T>(predicate: Fn(T) -> bool, items: Vec<T>) -> Vec<T>` | Returns elements where f is true. |
| [`filter_map`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn filter_map<T, U>(f: Fn(T) -> Option<U>, items: Vec<T>) -> Vec<U>` | Maps each element and keeps the Some results. |
| [`find`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn find<T>(predicate: Fn(T) -> bool, items: Vec<T>) -> Option<T>` | First element satisfying f, or None. |
| [`find_map`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn find_map<T, U>(f: Fn(T) -> Option<U>, items: Vec<T>) -> Option<U>` | First Some result of f over the sequence. |
| [`flat_map`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn flat_map<T, U>(f: Fn(T) -> Vec<U>, items: Vec<T>) -> Vec<U>` | Maps f and flattens one level. |
| [`flatten`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn flatten<T>(items: Vec<Vec<T>>) -> Vec<T>` | Flattens a Vec<Vec<T>> into Vec<T>. |
| [`fold`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn fold<T, U>(f: Fn(U, T) -> U, init: U, items: Vec<T>) -> U` | Reduces a sequence with an accumulator. |
| [`for_each`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn for_each<T>(f: Fn(T) -> (), items: Vec<T>) -> ()` | Applies f to each element for its side effect. |
| [`map`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn map<T, U>(f: Fn(T) -> U, items: Vec<T>) -> Vec<U>` | Applies f to each element, returning a new Vec. |
| [`max`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn max<T: Ord>(items: Vec<T>) -> Option<T>` | Largest element, or None when empty. |
| [`max_by`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn max_by<T>(compare: Fn(T, T) -> i64, items: Vec<T>) -> Option<T>` | Largest element by the comparison closure. |
| [`max_by_key`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn max_by_key<T, K: Ord>(key: Fn(T) -> K, items: Vec<T>) -> Option<T>` | Element with the largest derived key. |
| [`min`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn min<T: Ord>(items: Vec<T>) -> Option<T>` | Smallest element, or None when empty. |
| [`min_by`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn min_by<T>(compare: Fn(T, T) -> i64, items: Vec<T>) -> Option<T>` | Smallest element by the comparison closure. |
| [`min_by_key`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn min_by_key<T, K: Ord>(key: Fn(T) -> K, items: Vec<T>) -> Option<T>` | Element with the smallest derived key. |
| [`once`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn once<T>(value: T) -> Vec<T>` | Single-element Vec containing value. |
| [`pairwise`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn pairwise<T>(items: Vec<T>) -> Vec<(T, T)>` | Consecutive overlapping pairs. |
| [`partition`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn partition<T>(predicate: Fn(T) -> bool, items: Vec<T>) -> (Vec<T>, Vec<T>)` | Splits into (matching, non-matching) by f. |
| [`position`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn position<T>(predicate: Fn(T) -> bool, items: Vec<T>) -> Option<i64>` | Index of the first element satisfying f, or None. |
| [`product`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn product(items: Vec<i64>) -> i64` | Product of i64 or f64 elements. |
| [`product_by`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn product_by<T>(f: Fn(T) -> i64, items: Vec<T>) -> i64` | Product of f(element) over the sequence. |
| [`range`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn range(start: i64, end: i64) -> Vec<i64>` | Half-open integer sequence [start, end). |
| [`range_inclusive`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn range_inclusive(start: i64, end: i64) -> Vec<i64>` | Closed integer sequence [start, end]. |
| [`reduce`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn reduce<T>(f: Fn(T, T) -> T, items: Vec<T>) -> Option<T>` | Folds with the first element as the initial accumulator. |
| [`repeat`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn repeat<T>(value: T, count: i64) -> Vec<T>` | A value repeated n times. |
| [`rev`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn rev<T>(items: Vec<T>) -> Vec<T>` | Returns a rev copy. |
| [`scan`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn scan<T, S, U>(f: Fn(S, T) -> (S, Option<U>), state: S, items: Vec<T>) -> Vec<U>` | Folds while yielding each intermediate accumulator. |
| [`skip`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn skip<T>(n: i64, items: Vec<T>) -> Vec<T>` | All elements after the first n. |
| [`skip_while`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn skip_while<T>(predicate: Fn(T) -> bool, items: Vec<T>) -> Vec<T>` | Elements after the leading run satisfying f. |
| [`sort_by`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn sort_by<T>(compare: Fn(T, T) -> i64, items: Vec<T>) -> Vec<T>` | Sorted copy ordered by the comparison closure. |
| [`sort_by_key`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn sort_by_key<T, K: Ord>(key: Fn(T) -> K, items: Vec<T>) -> Vec<T>` | Sorted copy ordered by a derived key. |
| [`step_by`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn step_by<T>(step: i64, items: Vec<T>) -> Vec<T>` | Every step-th element, starting at index 0. |
| [`sum`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn sum(items: Vec<i64>) -> i64` | Sum of i64 or f64 elements. |
| [`sum_by`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn sum_by<T>(f: Fn(T) -> i64, items: Vec<T>) -> i64` | Sum of f(element) over the sequence. |
| [`take`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn take<T>(n: i64, items: Vec<T>) -> Vec<T>` | First n elements. |
| [`take_while`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn take_while<T>(predicate: Fn(T) -> bool, items: Vec<T>) -> Vec<T>` | Leading run of elements satisfying f. |
| [`unzip`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn unzip<A, B>(items: Vec<(A, B)>) -> (Vec<A>, Vec<B>)` | Splits a sequence of pairs into two Vecs. |
| [`windows`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn windows<T>(n: i64, items: Vec<T>) -> Vec<Vec<T>>` | Overlapping windows of width n. |
| [`zip`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/iter.rs) | `fn zip<A, B>(left: Vec<A>, right: Vec<B>) -> Vec<(A, B)>` | Pairs elements from two sequences. |
