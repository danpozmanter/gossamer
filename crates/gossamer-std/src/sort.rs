//! Runtime support for `std::sort` — stable / unstable sorting and
//! binary search.

#![forbid(unsafe_code)]

/// Sorts `slice` in ascending order using the item's `Ord` impl.
/// Uses the standard library's unstable sort, O(n log n) worst case.
pub fn sort<T: Ord>(slice: &mut [T]) {
    slice.sort_unstable();
}

/// Stable sort via the stdlib `sort`. Retains relative order of
/// equal elements.
pub fn sort_stable<T: Ord>(slice: &mut [T]) {
    slice.sort();
}

/// Sort by a comparator function.
pub fn sort_by<T, F>(slice: &mut [T], compare: F)
where
    F: FnMut(&T, &T) -> std::cmp::Ordering,
{
    slice.sort_by(compare);
}

/// Sort by a key-extraction function.
pub fn sort_by_key<T, K: Ord, F>(slice: &mut [T], key: F)
where
    F: FnMut(&T) -> K,
{
    slice.sort_by_key(key);
}

/// Returns the index where `target` is found, or where it would be
/// inserted to keep `slice` sorted. `slice` must already be sorted
/// ascending.
pub fn binary_search<T: Ord>(slice: &[T], target: &T) -> Result<usize, usize> {
    slice.binary_search(target)
}

/// Returns the first index `i` such that `predicate(&slice[i])` is
/// true. `slice` must be partitioned so that all `false` items come
/// before all `true` items.
pub fn partition_point<T, F>(slice: &[T], mut predicate: F) -> usize
where
    F: FnMut(&T) -> bool,
{
    let (mut lo, mut hi) = (0, slice.len());
    while lo < hi {
        let mid = usize::midpoint(lo, hi);
        if predicate(&slice[mid]) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_and_sort_stable_match_on_unique_keys() {
        let mut a = [3, 1, 4, 1, 5, 9, 2, 6];
        let mut b = a;
        sort(&mut a);
        sort_stable(&mut b);
        assert_eq!(a, b);
        assert_eq!(a, [1, 1, 2, 3, 4, 5, 6, 9]);
    }

    #[test]
    fn sort_by_key_uses_extractor() {
        let mut data: Vec<(i32, &'static str)> = vec![(2, "b"), (1, "a"), (3, "c")];
        sort_by_key(&mut data, |pair| pair.0);
        assert_eq!(data[0].0, 1);
        assert_eq!(data[2].0, 3);
    }

    #[test]
    fn binary_search_round_trip() {
        let slice = [1, 3, 5, 7, 9];
        assert_eq!(binary_search(&slice, &5), Ok(2));
        assert_eq!(binary_search(&slice, &4), Err(2));
    }

    #[test]
    fn partition_point_finds_boundary() {
        let slice = [1, 2, 3, 6, 7, 8];
        assert_eq!(partition_point(&slice, |x| *x < 5), 3);
    }
}
