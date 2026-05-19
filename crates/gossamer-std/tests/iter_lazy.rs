//! Regression coverage for the `std::iter` extensions:
//! - eager `sum` / `product` / `min` / `max` / `step_by` /
//!   `once` / `empty` / `collect` helpers.
//! - `Lazy` adapter — chains `map`/`filter`/`take`/`skip`/`step_by`
//!   without materialising intermediate `Vec`s.

use std::collections::HashSet;

use gossamer_std::iter;

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
    // Zero step is normalised to 1 — every element returned.
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
    assert_eq!(lazy_min, Some(1));
    assert_eq!(lazy_max, Some(9));
    assert_eq!(lazy_sum, 24);
    assert_eq!(lazy_count, 5);
}

#[test]
fn lazy_any_all_short_circuit() {
    let xs: Vec<i64> = vec![2, 4, 6, 7, 10];
    assert!(iter::Lazy::from(xs.iter().copied()).any(|n| n == 7));
    assert!(!iter::Lazy::from(xs.iter().copied()).all(|n| n % 2 == 0));
}
