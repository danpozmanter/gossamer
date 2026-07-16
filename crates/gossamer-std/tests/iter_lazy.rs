//! Regression coverage for the `std::iter` extensions:
//! - eager `sum` / `product` / `min` / `max` / `step_by` /
//!   `once` / `empty` / `collect` helpers.
//! - `Lazy` adapter - chains `map`/`filter`/`take`/`skip`/`enumerate`/
//!   `chain`/`zip`/`step_by`
//!   without materialising intermediate `Vec`s.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gossamer_std::iter;

#[derive(Clone)]
struct DropSpy(Arc<AtomicUsize>);

impl Drop for DropSpy {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

struct CloneSpy(Arc<AtomicUsize>);

impl Clone for CloneSpy {
    fn clone(&self) -> Self {
        self.0.fetch_add(1, Ordering::Relaxed);
        Self(self.0.clone())
    }
}

#[test]
fn sum_and_product_over_ints() {
    let xs = vec![1i64, 2, 3, 4, 5];
    assert_eq!(iter::sum::<i64>(&xs), 15);
    assert_eq!(iter::product::<i64>(&xs), 120);
}

#[test]
fn min_max_over_empty_and_nonempty() {
    let empty: Vec<i64> = Vec::new();
    assert_eq!(iter::min::<i64>(&empty), None);
    assert_eq!(iter::max::<i64>(&empty), None);
    let xs = vec![4i64, 1, 7, 3];
    assert_eq!(iter::min(&xs), Some(1));
    assert_eq!(iter::max(&xs), Some(7));
}

#[test]
fn step_by_returns_every_nth_element() {
    let xs = vec![1i64, 2, 3, 4, 5, 6, 7];
    assert_eq!(iter::step_by(2, &xs), vec![1, 3, 5, 7]);
    // Zero step is normalised to 1 - every element returned.
    assert_eq!(iter::step_by(0, &xs).len(), xs.len());
}

#[test]
fn once_and_empty_have_expected_shapes() {
    assert_eq!(iter::once(42i64), vec![42]);
    let e: Vec<i64> = iter::empty();
    assert!(e.is_empty());
}

#[test]
fn collect_into_hashset_via_turbofish() {
    let xs = vec![1i64, 2, 2, 3, 3, 3];
    let set: HashSet<i64> = iter::collect(xs);
    assert_eq!(set.len(), 3);
}

#[test]
fn lazy_chain_filters_and_maps_without_intermediates() {
    let xs: Vec<i64> = (1..=10).collect();
    // 1..=10 → keep evens → square → take first 3 → collect.
    let out: Vec<i64> = iter::Lazy::from(xs.iter().copied())
        .filter(|n| *n % 2 == 0)
        .map(|n| n * n)
        .take(3)
        .to_vec();
    assert_eq!(out, vec![4, 16, 36]);
}

#[test]
fn lazy_terminals_match_eager_values() {
    let xs: Vec<i64> = vec![5, 2, 9, 1, 7];
    let lazy_min = iter::Lazy::from(xs.iter().copied()).min();
    let lazy_max = iter::Lazy::from(xs.iter().copied()).max();
    let lazy_sum: i64 = iter::Lazy::from(xs.iter().copied()).sum();
    let lazy_count = iter::Lazy::from(xs.iter().copied()).count();
    let lazy_collected = iter::Lazy::from(xs.iter().copied()).collect();
    assert_eq!(lazy_min, Some(1));
    assert_eq!(lazy_max, Some(9));
    assert_eq!(lazy_sum, 24);
    assert_eq!(lazy_count, 5);
    assert_eq!(lazy_collected, xs);
}

#[test]
fn lazy_any_all_short_circuit() {
    let xs: Vec<i64> = vec![2, 4, 6, 7, 10];
    assert!(iter::Lazy::from(xs.iter().copied()).any(|n| n == 7));
    assert!(!iter::Lazy::from(xs.iter().copied()).all(|n| n % 2 == 0));
}

#[test]
fn lazy_enumerate_chain_and_zip_preserve_source_order() {
    let enumerated = iter::Lazy::from([10i64, 20].into_iter())
        .enumerate()
        .to_vec();
    assert_eq!(enumerated, vec![(0, 10), (1, 20)]);

    let chained = iter::Lazy::from([1i64, 2].into_iter())
        .chain(iter::Lazy::from([3, 4].into_iter()))
        .to_vec();
    assert_eq!(chained, vec![1, 2, 3, 4]);

    let zipped = iter::Lazy::from([1i64, 2, 3].into_iter())
        .zip(iter::Lazy::from([10i64, 20].into_iter()))
        .to_vec();
    assert_eq!(zipped, vec![(1, 10), (2, 20)]);
}

#[test]
fn lazy_find_short_circuits_after_the_decisive_item() {
    let pulls = AtomicUsize::new(0);
    let found = iter::Lazy::from((0i64..).inspect(|_| {
        pulls.fetch_add(1, Ordering::Relaxed);
    }))
    .map(|n| n * 2)
    .find(|n| *n == 8);

    assert_eq!(found, Some(8));
    assert_eq!(pulls.load(Ordering::Relaxed), 5);
}

#[test]
fn lazy_sources_are_range_borrowed_slice_and_owning_vec() {
    let range = iter::Lazy::range(2, 5).to_vec();
    assert_eq!(range, vec![2, 3, 4]);

    let source = [2i64, 3, 4];
    let borrowed = iter::Lazy::from_slice(&source).map(|n| n * 2).to_vec();
    assert_eq!(borrowed, vec![4, 6, 8]);
    assert_eq!(source, [2, 3, 4]);

    let owning = iter::Lazy::from_vec(vec![5i64, 6]).take(1).to_vec();
    assert_eq!(owning, vec![5]);
}

#[test]
fn borrowed_lazy_source_clones_only_items_that_are_pulled() {
    let clones = Arc::new(AtomicUsize::new(0));
    let source = [CloneSpy(clones.clone()), CloneSpy(clones.clone())];
    let pipeline = iter::Lazy::from_slice(&source).take(1);
    assert_eq!(clones.load(Ordering::Relaxed), 0);
    let output = pipeline.collect();
    assert_eq!(output.len(), 1);
    assert_eq!(clones.load(Ordering::Relaxed), 1);
}

#[test]
fn lazy_empty_and_bounded_infinite_sources_only_pull_when_consumed() {
    let empty_pulls = AtomicUsize::new(0);
    let empty = iter::Lazy::range(3, 3)
        .map(|n| {
            empty_pulls.fetch_add(1, Ordering::Relaxed);
            n
        })
        .collect();
    assert!(empty.is_empty());
    assert_eq!(empty_pulls.load(Ordering::Relaxed), 0);

    let bounded = iter::Lazy::from((0i64..).inspect(|_| {
        empty_pulls.fetch_add(1, Ordering::Relaxed);
    }))
    .take(3)
    .collect();
    assert_eq!(bounded, vec![0, 1, 2]);
    assert_eq!(empty_pulls.load(Ordering::Relaxed), 3);
}

#[test]
fn lazy_is_an_iterator_and_remains_exhausted() {
    let mut source = iter::Lazy::range(4, 6);
    assert_eq!(source.next(), Some(4));
    assert_eq!(source.next(), Some(5));
    assert_eq!(source.next(), None);
    assert_eq!(source.next(), None);
}

#[test]
fn lazy_mutable_closure_panic_and_drop_follow_pull_order() {
    let mut seen = Vec::new();
    let output = iter::Lazy::range(0, 5)
        .map(|n| {
            seen.push(n);
            n
        })
        .take(2)
        .to_vec();
    assert_eq!(output, vec![0, 1]);
    assert_eq!(seen, vec![0, 1]);

    let panic = std::panic::catch_unwind(|| {
        iter::Lazy::range(0, 4)
            .map(|n| if n == 2 { panic!("iterator panic") } else { n })
            .count()
    });
    assert!(panic.is_err());

    let drops = Arc::new(AtomicUsize::new(0));
    let items = (0..3).map(|_| DropSpy(drops.clone())).collect();
    drop(iter::Lazy::from_vec(items).take(1));
    assert_eq!(drops.load(Ordering::Relaxed), 3);
}
