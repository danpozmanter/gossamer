//! Sequence adapters for `std::iter`.
//!
//! All functions here operate on `Vec<T>` since that is Gossamer's growable
//! sequence type. In the interpreter these are exposed as variadic builtins
//! that accept a `Vec` value and a callable, matching SPEC §10.4.

#![forbid(unsafe_code)]

/// Returns the number of elements in `xs`.
#[must_use]
pub fn count<T>(xs: &[T]) -> usize {
    xs.len()
}

/// Returns the sum of all `i64` elements.
#[must_use]
pub fn sum_i64(xs: &[i64]) -> i64 {
    xs.iter().sum()
}

/// Returns the sum of all `f64` elements.
#[must_use]
pub fn sum_f64(xs: &[f64]) -> f64 {
    xs.iter().sum()
}

/// Returns the first `n` elements of `xs`.
#[must_use]
pub fn take<T: Clone>(xs: &[T], n: usize) -> Vec<T> {
    xs[..n.min(xs.len())].to_vec()
}

/// Drops the first `n` elements and returns the rest.
#[must_use]
pub fn skip<T: Clone>(xs: &[T], n: usize) -> Vec<T> {
    let start = n.min(xs.len());
    xs[start..].to_vec()
}

/// Zips two slices into a `Vec` of `(A, B)` pairs.
/// Stops at the shorter length.
#[must_use]
pub fn zip<A: Clone, B: Clone>(a: &[A], b: &[B]) -> Vec<(A, B)> {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x.clone(), y.clone()))
        .collect()
}

/// Pairs each element with its index: `[(0, a), (1, b), …]`.
#[must_use]
pub fn enumerate<T: Clone>(xs: &[T]) -> Vec<(usize, T)> {
    xs.iter().enumerate().map(|(i, x)| (i, x.clone())).collect()
}

/// Concatenates two slices into a single `Vec`.
#[must_use]
pub fn chain<T: Clone>(a: &[T], b: &[T]) -> Vec<T> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    out.extend_from_slice(a);
    out.extend_from_slice(b);
    out
}

/// Flattens a `Vec<Vec<T>>` into a `Vec<T>`.
#[must_use]
pub fn flatten<T: Clone>(xss: &[Vec<T>]) -> Vec<T> {
    xss.iter().flat_map(|v| v.iter().cloned()).collect()
}

/// Reverses a slice into a new `Vec`.
#[must_use]
pub fn reversed<T: Clone>(xs: &[T]) -> Vec<T> {
    xs.iter().rev().cloned().collect()
}

/// Deduplicate consecutive equal elements.
#[must_use]
pub fn dedup<T: Clone + PartialEq>(xs: &[T]) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    for x in xs {
        if out.last().is_none_or(|last| last != x) {
            out.push(x.clone());
        }
    }
    out
}

/// Returns `true` if every element satisfies the predicate `f`.
#[must_use]
pub fn all_i64(xs: &[i64], f: impl Fn(i64) -> bool) -> bool {
    xs.iter().all(|&x| f(x))
}

/// Returns `true` if any element satisfies the predicate `f`.
#[must_use]
pub fn any_i64(xs: &[i64], f: impl Fn(i64) -> bool) -> bool {
    xs.iter().any(|&x| f(x))
}

/// Folds `xs` with `init` and a binary `f(acc, elem)`.
#[must_use]
pub fn fold_i64(xs: &[i64], init: i64, f: impl Fn(i64, i64) -> i64) -> i64 {
    xs.iter().fold(init, |acc, &x| f(acc, x))
}

/// Maps `f` over `xs`, returning a new `Vec`.
#[must_use]
pub fn map_i64(xs: &[i64], f: impl Fn(i64) -> i64) -> Vec<i64> {
    xs.iter().map(|&x| f(x)).collect()
}

/// Filters `xs` by predicate `f`, returning a new `Vec`.
#[must_use]
pub fn filter_i64(xs: &[i64], f: impl Fn(i64) -> bool) -> Vec<i64> {
    xs.iter().filter(|&&x| f(x)).copied().collect()
}

/// Flat-maps `f` over `xs`, concatenating the results.
#[must_use]
pub fn flat_map_i64(xs: &[i64], f: impl Fn(i64) -> Vec<i64>) -> Vec<i64> {
    xs.iter().flat_map(|&x| f(x)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_and_skip() {
        let xs = vec![1i64, 2, 3, 4, 5];
        assert_eq!(take(&xs, 3), vec![1, 2, 3]);
        assert_eq!(skip(&xs, 2), vec![3, 4, 5]);
        assert_eq!(take(&xs, 10), xs);
        assert!(skip(&xs, 10).is_empty());
    }

    #[test]
    fn zip_stops_at_shorter() {
        let a = vec![1i64, 2, 3];
        let b = vec![4i64, 5];
        assert_eq!(zip(&a, &b), vec![(1, 4), (2, 5)]);
    }

    #[test]
    fn enumerate_pairs_with_index() {
        let xs = vec!['a', 'b', 'c'];
        assert_eq!(enumerate(&xs), vec![(0, 'a'), (1, 'b'), (2, 'c')]);
    }

    #[test]
    fn chain_concatenates() {
        let a = vec![1i64, 2];
        let b = vec![3i64, 4];
        assert_eq!(chain(&a, &b), vec![1, 2, 3, 4]);
    }

    #[test]
    fn flatten_collapses_nested() {
        let xss = vec![vec![1i64, 2], vec![3i64], vec![4i64, 5]];
        assert_eq!(flatten(&xss), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn fold_sum() {
        let xs = vec![1i64, 2, 3, 4, 5];
        assert_eq!(fold_i64(&xs, 0, |acc, x| acc + x), 15);
    }

    #[test]
    fn map_doubles() {
        let xs = vec![1i64, 2, 3];
        assert_eq!(map_i64(&xs, |x| x * 2), vec![2, 4, 6]);
    }

    #[test]
    fn filter_evens() {
        let xs = vec![1i64, 2, 3, 4, 5];
        assert_eq!(filter_i64(&xs, |x| x % 2 == 0), vec![2, 4]);
    }

    #[test]
    fn all_any_semantics() {
        let xs = vec![2i64, 4, 6];
        assert!(all_i64(&xs, |x| x % 2 == 0));
        assert!(!any_i64(&xs, |x| x % 2 != 0));
    }

    #[test]
    fn dedup_consecutive() {
        let xs = vec![1i64, 1, 2, 2, 3, 1];
        assert_eq!(dedup(&xs), vec![1, 2, 3, 1]);
    }

    #[test]
    fn count_returns_len() {
        assert_eq!(count(&[1i64, 2, 3]), 3);
    }
}
