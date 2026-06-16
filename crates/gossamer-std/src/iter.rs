//! Sequence combinators for `std::iter`.
//!
//! Generic, eager, data-last. Every closure-taking combinator in this
//! module takes the callable first and the data last, mirroring F#'s
//! `Seq`/`List`/`Array` modules so `xs |> iter::map(f)` desugars
//! (per SPEC §4.6) to `iter::map(f, xs)` and threads cleanly.
//!
//! These Rust-side helpers exist for stdlib code that wants to call
//! into the same surface; user `.gos` programs see the dynamic
//! `Value`-typed wrappers in `crates/gossamer-interp/src/
//! stdlib_builtins.rs::install_iter`.

#![forbid(unsafe_code)]

use std::cmp::Ordering;
use std::collections::HashMap;
use std::hash::Hash;

/// Returns the number of elements in `xs`.
#[must_use]
pub fn count<T>(xs: &[T]) -> usize {
    xs.len()
}

/// Sums every element via the type's `Add` impl. Mirrors
/// Python's `sum`. Empty input returns `T::default()`.
#[must_use]
pub fn sum<T>(xs: &[T]) -> T
where
    T: Clone + std::iter::Sum<T>,
{
    xs.iter().cloned().sum()
}

/// Product of every element. Mirrors Python's `math.prod`.
#[must_use]
pub fn product<T>(xs: &[T]) -> T
where
    T: Clone + std::iter::Product<T>,
{
    xs.iter().cloned().product()
}

/// Returns the smallest element by `Ord`, or `None` for empty
/// input. Mirrors Python's `min`.
#[must_use]
pub fn min<T: Clone + Ord>(xs: &[T]) -> Option<T> {
    xs.iter().min().cloned()
}

/// Returns the largest element by `Ord`. Mirrors Python's `max`.
#[must_use]
pub fn max<T: Clone + Ord>(xs: &[T]) -> Option<T> {
    xs.iter().max().cloned()
}

/// Returns every `step`-th element starting from index 0. A
/// `step` of zero is normalised to 1 to avoid the otherwise
/// infinite loop. Mirrors Python's slice-step
/// (`xs[::step]`) and Rust's `Iterator::step_by`.
#[must_use]
pub fn step_by<T: Clone>(step: usize, xs: &[T]) -> Vec<T> {
    let s = step.max(1);
    xs.iter().step_by(s).cloned().collect()
}

/// `[item]` - single-element vec. Useful as an `iter::flat_map`
/// terminator when only one of many call-paths emits a value.
#[must_use]
pub fn once<T>(item: T) -> Vec<T> {
    vec![item]
}

/// Zero-length vec of the type `T`. Pairs with `flat_map` to
/// "skip" an entry without `filter_map`'s `Option` wrapping.
#[must_use]
pub fn empty<T>() -> Vec<T> {
    Vec::new()
}

/// Collects every element into a concrete container. The target
/// type is selected via turbofish at the call site, e.g.
/// `collect::<HashSet<_>>(xs)`. Mirrors Rust's `Iterator::collect`.
#[must_use]
pub fn collect<T, C>(xs: Vec<T>) -> C
where
    C: FromIterator<T>,
{
    xs.into_iter().collect()
}

/// Lazy iterator adapter wrapping any [`Iterator`]. Builds a
/// chain without materialising intermediate `Vec`s.
///
/// `Lazy::from(xs.iter().cloned())` is the canonical entry; the
/// chain terminates via [`Lazy::to_vec`], [`Lazy::sum`],
/// [`Lazy::min`], [`Lazy::max`], [`Lazy::count`], [`Lazy::first`],
/// or any of the boolean reducers.
pub struct Lazy<I: Iterator> {
    inner: I,
}

impl<I: Iterator> Lazy<I> {
    /// Wraps an existing iterator.
    pub fn from(inner: I) -> Self {
        Self { inner }
    }

    /// Lazy `map` adapter - yields `f(item)` per pull.
    pub fn map<U, F: FnMut(I::Item) -> U>(self, f: F) -> Lazy<std::iter::Map<I, F>> {
        Lazy {
            inner: self.inner.map(f),
        }
    }

    /// Lazy `filter` adapter.
    pub fn filter<P: FnMut(&I::Item) -> bool>(self, p: P) -> Lazy<std::iter::Filter<I, P>> {
        Lazy {
            inner: self.inner.filter(p),
        }
    }

    /// Lazy `take` adapter - yields at most `n` elements.
    pub fn take(self, n: usize) -> Lazy<std::iter::Take<I>> {
        Lazy {
            inner: self.inner.take(n),
        }
    }

    /// Lazy `skip` adapter - drops the first `n` elements.
    pub fn skip(self, n: usize) -> Lazy<std::iter::Skip<I>> {
        Lazy {
            inner: self.inner.skip(n),
        }
    }

    /// Lazy `step_by` adapter. `n` of zero is normalised to 1.
    pub fn step_by(self, n: usize) -> Lazy<std::iter::StepBy<I>> {
        Lazy {
            inner: self.inner.step_by(n.max(1)),
        }
    }

    /// Terminal - collects every yielded element into a `Vec`.
    pub fn to_vec(self) -> Vec<I::Item> {
        self.inner.collect()
    }

    /// Terminal - sums every element.
    pub fn sum<S: std::iter::Sum<I::Item>>(self) -> S {
        self.inner.sum()
    }

    /// Terminal - multiplies every element.
    pub fn product<P: std::iter::Product<I::Item>>(self) -> P {
        self.inner.product()
    }

    /// Terminal - smallest element by `Ord`.
    pub fn min(self) -> Option<I::Item>
    where
        I::Item: Ord,
    {
        self.inner.min()
    }

    /// Terminal - largest element by `Ord`.
    pub fn max(self) -> Option<I::Item>
    where
        I::Item: Ord,
    {
        self.inner.max()
    }

    /// Terminal - number of yielded elements.
    pub fn count(self) -> usize {
        self.inner.count()
    }

    /// Terminal - first element, if any.
    pub fn first(mut self) -> Option<I::Item> {
        self.inner.next()
    }

    /// Terminal - fold.
    pub fn fold<A, F: FnMut(A, I::Item) -> A>(self, init: A, f: F) -> A {
        self.inner.fold(init, f)
    }

    /// Terminal - `true` iff every element satisfies `p`.
    pub fn all<P: FnMut(I::Item) -> bool>(mut self, mut p: P) -> bool {
        self.inner.all(&mut p)
    }

    /// Terminal - `true` iff at least one element satisfies `p`.
    pub fn any<P: FnMut(I::Item) -> bool>(mut self, mut p: P) -> bool {
        self.inner.any(&mut p)
    }
}

/// Returns the first `n` elements of `xs`.
#[must_use]
pub fn take<T: Clone>(n: usize, xs: &[T]) -> Vec<T> {
    xs[..n.min(xs.len())].to_vec()
}

/// Drops the first `n` elements and returns the rest.
#[must_use]
pub fn skip<T: Clone>(n: usize, xs: &[T]) -> Vec<T> {
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

/// Run `f` for every element of `xs`. Returns `()`.
pub fn for_each<T: Clone, F: FnMut(T)>(mut f: F, xs: &[T]) {
    for x in xs {
        f(x.clone());
    }
}

/// Map `f` over `xs`, returning a new `Vec`.
#[must_use]
pub fn map<T: Clone, U, F: FnMut(T) -> U>(mut f: F, xs: &[T]) -> Vec<U> {
    xs.iter().map(|x| f(x.clone())).collect()
}

/// Filter `xs` by predicate `p`.
#[must_use]
pub fn filter<T: Clone, P: FnMut(&T) -> bool>(mut p: P, xs: &[T]) -> Vec<T> {
    xs.iter().filter(|x| p(*x)).cloned().collect()
}

/// Apply `f`, keeping only the `Some` results.
#[must_use]
pub fn filter_map<T: Clone, U, F: FnMut(T) -> Option<U>>(mut f: F, xs: &[T]) -> Vec<U> {
    xs.iter().filter_map(|x| f(x.clone())).collect()
}

/// Apply `f` and concatenate the resulting `Vec`s.
#[must_use]
pub fn flat_map<T: Clone, U, F: FnMut(T) -> Vec<U>>(mut f: F, xs: &[T]) -> Vec<U> {
    xs.iter().flat_map(|x| f(x.clone())).collect()
}

/// Fold `xs` with `init` and a binary `f(acc, elem)`.
pub fn fold<T: Clone, A, F: FnMut(A, T) -> A>(init: A, mut f: F, xs: &[T]) -> A {
    let mut acc = init;
    for x in xs {
        acc = f(acc, x.clone());
    }
    acc
}

/// Reduce `xs` with `f`. Returns `None` if `xs` is empty.
pub fn reduce<T: Clone, F: FnMut(T, T) -> T>(f: F, xs: &[T]) -> Option<T> {
    let mut iter = xs.iter().cloned();
    let first = iter.next()?;
    Some(iter.fold(first, f))
}

/// Scan `xs` left-to-right, emitting each intermediate accumulator.
pub fn scan<T: Clone, A: Clone, F: FnMut(A, T) -> A>(init: A, mut f: F, xs: &[T]) -> Vec<A> {
    let mut out = Vec::with_capacity(xs.len());
    let mut acc = init;
    for x in xs {
        acc = f(acc.clone(), x.clone());
        out.push(acc.clone());
    }
    out
}

/// Returns `true` if any element satisfies `p`.
pub fn any<T: Clone, P: FnMut(&T) -> bool>(p: P, xs: &[T]) -> bool {
    xs.iter().any(p)
}

/// Returns `true` if every element satisfies `p`.
pub fn all<T: Clone, P: FnMut(&T) -> bool>(p: P, xs: &[T]) -> bool {
    xs.iter().all(p)
}

/// First element satisfying `p`.
pub fn find<T: Clone, P: FnMut(&T) -> bool>(mut p: P, xs: &[T]) -> Option<T> {
    xs.iter().find(|x| p(*x)).cloned()
}

/// Index of the first element satisfying `p`.
pub fn position<T, P: FnMut(&T) -> bool>(p: P, xs: &[T]) -> Option<usize> {
    xs.iter().position(p)
}

/// First `Some(_)` result of applying `f`.
pub fn find_map<T: Clone, U, F: FnMut(T) -> Option<U>>(mut f: F, xs: &[T]) -> Option<U> {
    for x in xs {
        if let Some(v) = f(x.clone()) {
            return Some(v);
        }
    }
    None
}

/// Take the longest prefix where `p` holds.
#[must_use]
pub fn take_while<T: Clone, P: FnMut(&T) -> bool>(mut p: P, xs: &[T]) -> Vec<T> {
    xs.iter().take_while(|x| p(x)).cloned().collect()
}

/// Drop the longest prefix where `p` holds; return the rest.
#[must_use]
pub fn skip_while<T: Clone, P: FnMut(&T) -> bool>(mut p: P, xs: &[T]) -> Vec<T> {
    xs.iter().skip_while(|x| p(x)).cloned().collect()
}

/// Partition `xs` into `(matches, non_matches)`.
#[must_use]
pub fn partition<T: Clone, P: FnMut(&T) -> bool>(mut p: P, xs: &[T]) -> (Vec<T>, Vec<T>) {
    xs.iter().cloned().partition(|x| p(x))
}

/// Sort `xs` via a comparator into a new `Vec`.
#[must_use]
pub fn sort_by<T: Clone, F: FnMut(&T, &T) -> Ordering>(mut cmp: F, xs: &[T]) -> Vec<T> {
    let mut out = xs.to_vec();
    out.sort_by(|a, b| cmp(a, b));
    out
}

/// Sort `xs` by a derived key into a new `Vec`.
#[must_use]
pub fn sort_by_key<T: Clone, K: Ord, F: FnMut(&T) -> K>(mut key: F, xs: &[T]) -> Vec<T> {
    let mut out = xs.to_vec();
    out.sort_by_key(|x| key(x));
    out
}

/// Group elements by a derived key, preserving insertion order in
/// the value vectors.
#[must_use]
pub fn group_by<T: Clone, K: Hash + Eq, F: FnMut(&T) -> K>(
    mut key: F,
    xs: &[T],
) -> HashMap<K, Vec<T>> {
    let mut out: HashMap<K, Vec<T>> = HashMap::new();
    for x in xs {
        out.entry(key(x)).or_default().push(x.clone());
    }
    out
}

/// Count occurrences of each derived key.
#[must_use]
pub fn count_by<T, K: Hash + Eq, F: FnMut(&T) -> K>(mut key: F, xs: &[T]) -> HashMap<K, i64> {
    let mut out: HashMap<K, i64> = HashMap::new();
    for x in xs {
        *out.entry(key(x)).or_insert(0) += 1;
    }
    out
}

/// Sliding windows of width `n`. Empty if `n == 0` or `xs.len() < n`.
#[must_use]
pub fn windowed<T: Clone>(n: usize, xs: &[T]) -> Vec<Vec<T>> {
    if n == 0 || xs.len() < n {
        return Vec::new();
    }
    xs.windows(n).map(<[T]>::to_vec).collect()
}

/// Successive overlapping pairs: `[(a,b), (b,c), (c,d), …]`.
#[must_use]
pub fn pairwise<T: Clone>(xs: &[T]) -> Vec<(T, T)> {
    xs.windows(2)
        .map(|w| (w[0].clone(), w[1].clone()))
        .collect()
}

/// Split `xs` into consecutive chunks of size `n`. Final chunk may
/// be shorter. `n == 0` returns empty.
#[must_use]
pub fn chunk_by_size<T: Clone>(n: usize, xs: &[T]) -> Vec<Vec<T>> {
    if n == 0 {
        return Vec::new();
    }
    xs.chunks(n).map(<[T]>::to_vec).collect()
}

/// Inclusive-exclusive range `[start, end)`.
#[must_use]
pub fn range(start: i64, end: i64) -> Vec<i64> {
    if end <= start {
        return Vec::new();
    }
    (start..end).collect()
}

/// Inclusive-inclusive range `[start, end]`.
#[must_use]
pub fn range_inclusive(start: i64, end: i64) -> Vec<i64> {
    if end < start {
        return Vec::new();
    }
    (start..=end).collect()
}

/// Repeat `v` exactly `n` times.
#[must_use]
pub fn repeat<T: Clone>(v: T, n: usize) -> Vec<T> {
    vec![v; n]
}

/// Unzip a slice of pairs into two vectors.
#[must_use]
pub fn unzip<A: Clone, B: Clone>(pairs: &[(A, B)]) -> (Vec<A>, Vec<B>) {
    let mut a = Vec::with_capacity(pairs.len());
    let mut b = Vec::with_capacity(pairs.len());
    for (x, y) in pairs {
        a.push(x.clone());
        b.push(y.clone());
    }
    (a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_and_skip() {
        let xs = vec![1i64, 2, 3, 4, 5];
        assert_eq!(take(3, &xs), vec![1, 2, 3]);
        assert_eq!(skip(2, &xs), vec![3, 4, 5]);
        assert_eq!(take(10, &xs), xs);
        assert!(skip(10, &xs).is_empty());
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
        assert_eq!(fold(0i64, |acc, x| acc + x, &xs), 15);
    }

    #[test]
    fn reduce_empty_is_none() {
        let xs: Vec<i64> = vec![];
        assert!(reduce(|a, b| a + b, &xs).is_none());
        assert_eq!(reduce(|a, b| a + b, &[1i64, 2, 3]), Some(6));
    }

    #[test]
    fn map_doubles() {
        let xs = vec![1i64, 2, 3];
        assert_eq!(map(|x| x * 2, &xs), vec![2, 4, 6]);
    }

    #[test]
    fn filter_evens() {
        let xs = vec![1i64, 2, 3, 4, 5];
        assert_eq!(filter(|x| *x % 2 == 0, &xs), vec![2, 4]);
    }

    #[test]
    fn filter_map_extracts_some() {
        let xs = vec![1i64, 2, 3, 4];
        assert_eq!(
            filter_map(|x| if x % 2 == 0 { Some(x * 10) } else { None }, &xs),
            vec![20, 40]
        );
    }

    #[test]
    fn all_any_semantics() {
        let xs = vec![2i64, 4, 6];
        assert!(all(|x| *x % 2 == 0, &xs));
        assert!(!any(|x| *x % 2 != 0, &xs));
    }

    #[test]
    fn find_and_position() {
        let xs = vec![1i64, 3, 5, 8, 11];
        assert_eq!(find(|x| *x > 4, &xs), Some(5));
        assert_eq!(position(|x| *x > 4, &xs), Some(2));
        assert_eq!(find(|x| *x > 100, &xs), None);
    }

    #[test]
    fn take_skip_while() {
        let xs = vec![1i64, 2, 3, 4, 1];
        assert_eq!(take_while(|x| *x < 3, &xs), vec![1, 2]);
        assert_eq!(skip_while(|x| *x < 3, &xs), vec![3, 4, 1]);
    }

    #[test]
    fn partition_splits() {
        let xs = vec![1i64, 2, 3, 4];
        assert_eq!(partition(|x| *x % 2 == 0, &xs), (vec![2, 4], vec![1, 3]));
    }

    #[test]
    fn sort_by_key_orders() {
        let xs = vec![3i64, 1, 4, 1, 5, 9, 2, 6];
        assert_eq!(sort_by_key(|x| -*x, &xs), vec![9, 6, 5, 4, 3, 2, 1, 1]);
    }

    #[test]
    fn group_by_partitions() {
        let xs = vec![1i64, 2, 3, 4, 5];
        let grouped = group_by(|x| *x % 2, &xs);
        assert_eq!(grouped.get(&1), Some(&vec![1, 3, 5]));
        assert_eq!(grouped.get(&0), Some(&vec![2, 4]));
    }

    #[test]
    fn count_by_counts() {
        let xs = vec!["a", "b", "a", "a", "c"];
        let counts = count_by(std::string::ToString::to_string, &xs);
        assert_eq!(counts.get("a"), Some(&3));
        assert_eq!(counts.get("b"), Some(&1));
        assert_eq!(counts.get("c"), Some(&1));
    }

    #[test]
    fn windowed_and_pairwise() {
        let xs = vec![1i64, 2, 3, 4];
        assert_eq!(windowed(2, &xs), vec![vec![1, 2], vec![2, 3], vec![3, 4]]);
        assert_eq!(pairwise(&xs), vec![(1, 2), (2, 3), (3, 4)]);
        assert!(windowed(0, &xs).is_empty());
        assert!(windowed(5, &xs).is_empty());
    }

    #[test]
    fn chunk_by_size_splits() {
        let xs = vec![1i64, 2, 3, 4, 5];
        assert_eq!(chunk_by_size(2, &xs), vec![vec![1, 2], vec![3, 4], vec![5]]);
        assert!(chunk_by_size(0, &xs).is_empty());
    }

    #[test]
    fn range_and_range_inclusive() {
        assert_eq!(range(1, 4), vec![1, 2, 3]);
        assert_eq!(range_inclusive(1, 4), vec![1, 2, 3, 4]);
        assert!(range(5, 3).is_empty());
    }

    #[test]
    fn repeat_and_unzip() {
        assert_eq!(repeat(7i64, 3), vec![7, 7, 7]);
        let pairs = vec![(1i64, 'a'), (2, 'b'), (3, 'c')];
        assert_eq!(unzip(&pairs), (vec![1, 2, 3], vec!['a', 'b', 'c']));
    }

    #[test]
    fn scan_emits_intermediates() {
        let xs = vec![1i64, 2, 3, 4];
        assert_eq!(scan(0i64, |acc, x| acc + x, &xs), vec![1, 3, 6, 10]);
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

    #[test]
    fn for_each_visits_all() {
        let xs = vec![1i64, 2, 3];
        let mut sum = 0i64;
        for_each(|x| sum += x, &xs);
        assert_eq!(sum, 6);
    }
}
