// `std::collections::ordered_vec` - sorted-on-insert `Vec<i64>` with
// binary-search lookups.
//
// (Previously also exposed `list` and `ordered_list`. Both were
// removed in 0.7.0 because they duplicated `Vec` / `ordered_vec`
// behaviour without adding anything - the linked-list shape was
// emulated over `Vec` anyway.)

#![forbid(unsafe_code)]
#![allow(
    missing_docs,
    reason = "trivial container ops mirror the canonical names"
)]

/// Sorted-on-insert `Vec<i64>`.
pub mod ordered_vec {
    /// Insert `value` at its unique sorted position.
    #[must_use]
    pub fn insert(mut xs: Vec<i64>, value: i64) -> Vec<i64> {
        let pos = xs.partition_point(|&x| x < value);
        xs.insert(pos, value);
        xs
    }

    /// Remove the element at index `i`.
    #[must_use]
    pub fn remove_at(mut xs: Vec<i64>, i: i64) -> Vec<i64> {
        if i >= 0 && (i as usize) < xs.len() {
            xs.remove(i as usize);
        }
        xs
    }

    #[must_use]
    pub fn contains(xs: &[i64], value: i64) -> bool {
        xs.binary_search(&value).is_ok()
    }

    #[must_use]
    pub fn index_of(xs: &[i64], value: i64) -> i64 {
        xs.binary_search(&value).map_or(-1, |i| i as i64)
    }

    #[must_use]
    pub fn peek_min(xs: &[i64]) -> i64 {
        xs.first().copied().unwrap_or(0)
    }
    #[must_use]
    pub fn peek_max(xs: &[i64]) -> i64 {
        xs.last().copied().unwrap_or(0)
    }
    #[must_use]
    pub fn len(xs: &[i64]) -> i64 {
        xs.len() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_vec_stays_sorted() {
        let v = vec![];
        let v = ordered_vec::insert(v, 3);
        let v = ordered_vec::insert(v, 1);
        let v = ordered_vec::insert(v, 4);
        let v = ordered_vec::insert(v, 1);
        let v = ordered_vec::insert(v, 5);
        assert_eq!(v, vec![1, 1, 3, 4, 5]);
        assert!(ordered_vec::contains(&v, 4));
        assert!(!ordered_vec::contains(&v, 99));
        assert_eq!(ordered_vec::peek_min(&v), 1);
        assert_eq!(ordered_vec::peek_max(&v), 5);
    }
}
