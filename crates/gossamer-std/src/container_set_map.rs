// `std::container::ordered_set` - sorted Vec<i64> with dedup on
// insert. `std::container::ordered_map` - flat [k, v, k, v, ...]
// pair Vec sorted by key. Both use the rebind shape.

#![forbid(unsafe_code)]
#![allow(
    missing_docs,
    reason = "trivial container ops mirror the canonical names"
)]

/// Sorted set of `i64` with dedup on insert.
pub mod ordered_set {
    /// Insert `value` keeping sort order; no-op if already present.
    #[must_use]
    pub fn insert(mut xs: Vec<i64>, value: i64) -> Vec<i64> {
        match xs.binary_search(&value) {
            Ok(_) => xs,
            Err(pos) => {
                xs.insert(pos, value);
                xs
            }
        }
    }

    #[must_use]
    pub fn remove(mut xs: Vec<i64>, value: i64) -> Vec<i64> {
        if let Ok(pos) = xs.binary_search(&value) {
            xs.remove(pos);
        }
        xs
    }

    #[must_use]
    pub fn contains(xs: &[i64], value: i64) -> bool {
        xs.binary_search(&value).is_ok()
    }

    #[must_use]
    pub fn len(xs: &[i64]) -> i64 {
        xs.len() as i64
    }
}

/// Sorted i64 -> i64 map backed by a flat pair `Vec<i64>`.
pub mod ordered_map {
    /// Insert `key => value`; replaces if key exists.
    #[must_use]
    pub fn insert(mut xs: Vec<i64>, key: i64, value: i64) -> Vec<i64> {
        let pairs = xs.len() / 2;
        for i in 0..pairs {
            let k = xs[i * 2];
            if k == key {
                xs[i * 2 + 1] = value;
                return xs;
            }
            if k > key {
                xs.insert(i * 2, key);
                xs.insert(i * 2 + 1, value);
                return xs;
            }
        }
        xs.push(key);
        xs.push(value);
        xs
    }

    #[must_use]
    pub fn remove(mut xs: Vec<i64>, key: i64) -> Vec<i64> {
        let pairs = xs.len() / 2;
        for i in 0..pairs {
            if xs[i * 2] == key {
                xs.remove(i * 2);
                xs.remove(i * 2);
                return xs;
            }
        }
        xs
    }

    /// Lookup `key`; returns 0 if not found.
    #[must_use]
    pub fn get(xs: &[i64], key: i64) -> i64 {
        let pairs = xs.len() / 2;
        for i in 0..pairs {
            if xs[i * 2] == key {
                return xs[i * 2 + 1];
            }
        }
        0
    }

    #[must_use]
    pub fn contains_key(xs: &[i64], key: i64) -> bool {
        let pairs = xs.len() / 2;
        for i in 0..pairs {
            if xs[i * 2] == key {
                return true;
            }
        }
        false
    }

    #[must_use]
    pub fn len(xs: &[i64]) -> i64 {
        (xs.len() / 2) as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oset_dedup() {
        let s = vec![];
        let s = ordered_set::insert(s, 3);
        let s = ordered_set::insert(s, 1);
        let s = ordered_set::insert(s, 2);
        let s = ordered_set::insert(s, 1);
        assert_eq!(s, vec![1, 2, 3]);
        assert!(ordered_set::contains(&s, 2));
        let s = ordered_set::remove(s, 2);
        assert_eq!(s, vec![1, 3]);
    }

    #[test]
    fn omap_basic() {
        let m = vec![];
        let m = ordered_map::insert(m, 10, 100);
        let m = ordered_map::insert(m, 5, 50);
        let m = ordered_map::insert(m, 7, 70);
        // Sorted by key: [5,50, 7,70, 10,100]
        assert_eq!(m, vec![5, 50, 7, 70, 10, 100]);
        assert_eq!(ordered_map::get(&m, 7), 70);
        assert_eq!(ordered_map::len(&m), 3);
        let m = ordered_map::insert(m, 7, 700);
        assert_eq!(ordered_map::get(&m, 7), 700);
        let m = ordered_map::remove(m, 5);
        assert_eq!(ordered_map::len(&m), 2);
    }
}
