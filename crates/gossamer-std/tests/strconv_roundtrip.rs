//! `strconv` parse → format → parse round-trip pin tests.
//!
//! The existing phase tests cover individual parse / format
//! correctness. This file pins the round-trip semantics: a
//! number formatted by `format_*` and then re-parsed by
//! `parse_*` must yield the original value bitwise (for ints)
//! or within float epsilon (for f64). The regression class is
//! formatter / parser asymmetry.

#![allow(missing_docs)]

use gossamer_std::strconv::{format_f64, format_i64, parse_f64, parse_i64};

#[test]
fn i64_round_trip_over_representative_values() {
    let values = [
        0_i64,
        1,
        -1,
        42,
        -42,
        1_000_000,
        -1_000_000,
        i64::MAX,
        i64::MIN,
        i64::MAX - 1,
        i64::MIN + 1,
    ];
    for &n in &values {
        let s = format_i64(n);
        let back = parse_i64(&s).unwrap_or_else(|e| panic!("parse {s:?}: {e:?}"));
        assert_eq!(
            back, n,
            "i64 round-trip diverged for {n} (formatted as {s:?})"
        );
    }
}

#[test]
fn f64_round_trip_for_finite_values() {
    let values = [
        0.0_f64,
        1.0,
        -1.0,
        std::f64::consts::PI,
        -std::f64::consts::E,
        1e10,
        1e-10,
        f64::EPSILON,
        f64::MAX,
        f64::MIN_POSITIVE,
    ];
    for &n in &values {
        let s = format_f64(n);
        let back = parse_f64(&s).unwrap_or_else(|e| panic!("parse {s:?}: {e:?}"));
        // Use bit-pattern equality where possible; some
        // representations may round-trip with negligible drift
        // (within 1 ulp). We treat exact equality as the bar
        // here — anything else flags a real precision loss.
        assert_eq!(
            back.to_bits(),
            n.to_bits(),
            "f64 round-trip diverged for {n} (formatted as {s:?}, back as {back})",
        );
    }
}

#[test]
fn parse_i64_rejects_obvious_garbage() {
    assert!(parse_i64("abc").is_err());
    assert!(parse_i64("").is_err());
    assert!(parse_i64("1.5").is_err());
    assert!(parse_i64("12 34").is_err());
}

#[test]
fn parse_i64_accepts_explicit_plus_sign() {
    // The phase tests don't pin the leading-plus shape; a
    // parser regression that drops `+` recognition surfaces
    // here.
    assert_eq!(parse_i64("+42").unwrap(), 42);
}

#[test]
fn parse_f64_handles_scientific_notation_round_trip() {
    let s = format_f64(1.5e10_f64);
    let back = parse_f64(&s).unwrap();
    assert_eq!(back, 1.5e10);
}
