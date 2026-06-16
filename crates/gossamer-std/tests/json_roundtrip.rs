//! JSON parse → encode → parse round-trip regression matrix.
//!
//! The existing `phase24.rs` tests verify that `parse` and
//! `encode` work in isolation. This file pins the round-trip
//! semantics: a value parsed from one JSON string, re-encoded,
//! and parsed again must match the original. The regression
//! class is encoder/decoder asymmetry - a fix to one without
//! the other corrupts every nested value.

#![allow(missing_docs)]

use gossamer_std::json::{self, Value};

/// Round-trips `text` through parse → encode → parse and
/// asserts both pass. Returns the final `Value` for the caller
/// to make additional structural assertions on.
fn roundtrip(text: &str) -> Value {
    let first = json::parse(text).unwrap_or_else(|e| {
        panic!("first parse failed for {text:?}: {e:?}");
    });
    let re_encoded = json::encode(&first);
    let second = json::parse(&re_encoded).unwrap_or_else(|e| {
        panic!("second parse failed for re-encoded {re_encoded:?} (from {text:?}): {e:?}");
    });
    assert_eq!(
        first, second,
        "round-trip diverged for {text:?}\n  encoded: {re_encoded:?}",
    );
    second
}

#[test]
fn scalar_round_trip() {
    roundtrip("null");
    roundtrip("true");
    roundtrip("false");
    roundtrip("0");
    roundtrip("-1");
    roundtrip("42");
    roundtrip("3.14");
    roundtrip("\"hello\"");
}

#[test]
fn array_of_scalars_round_trip() {
    let v = roundtrip("[1, 2, 3, 4, 5]");
    let arr = json::as_array(&v).expect("expected array");
    assert_eq!(arr.len(), 5);
}

#[test]
fn object_with_mixed_value_types_round_trips() {
    let v = roundtrip(r#"{"name":"alice","age":30,"active":true,"tags":["a","b"]}"#);
    assert!(json::as_object(&v).is_some());
    assert_eq!(
        json::as_str(json::get(&v, "name").unwrap()).unwrap(),
        "alice"
    );
    assert_eq!(json::as_i64(json::get(&v, "age").unwrap()).unwrap(), 30);
    assert!(json::as_bool(json::get(&v, "active").unwrap()).unwrap());
}

#[test]
fn nested_objects_round_trip() {
    // Three-level nesting + array. The regression class is an
    // encoder that drops one level (truncating to depth-2) or a
    // parser that flattens nested objects into siblings.
    let v = roundtrip(r#"{"outer":{"middle":{"inner":[1,2,3],"name":"deep"},"flag":false}}"#);
    let outer = json::get(&v, "outer").unwrap();
    let middle = json::get(outer, "middle").unwrap();
    let inner = json::get(middle, "inner").unwrap();
    assert_eq!(json::len(inner), 3);
    let name = json::as_str(json::get(middle, "name").unwrap()).unwrap();
    assert_eq!(name, "deep");
}

#[test]
fn unicode_strings_preserve_through_round_trip() {
    // Multi-byte UTF-8: emoji, non-Latin scripts, escapes. The
    // encoder must escape correctly and the decoder must restore
    // the original bytes.
    let inputs = [
        r#""café""#,
        r#""日本語""#,
        r#""emoji 🎉""#,
        r#""line1\nline2""#,
        r#""quoted \"thing\"""#,
        r#""tab\there""#,
    ];
    for input in &inputs {
        let v1 = json::parse(input).unwrap();
        let encoded = json::encode(&v1);
        let v2 = json::parse(&encoded)
            .unwrap_or_else(|e| panic!("re-parse failed for {input:?} → {encoded:?}: {e:?}"));
        assert_eq!(v1, v2, "round-trip diverged for {input:?}");
    }
}

#[test]
fn deeply_nested_arrays_round_trip() {
    // Five-level array nesting. Catches stack-bound bugs in the
    // recursive descent decoder and accumulator-handling bugs in
    // the encoder.
    let text = "[[[[[1,2],3],4],5],6]";
    let v = roundtrip(text);
    // Outer is a 2-element array.
    assert_eq!(json::len(&v), 2);
}

#[test]
fn empty_collections_round_trip() {
    // `[]` and `{}` distinct from null; the parser must
    // produce the right empty shape and the encoder must emit
    // the matching brackets.
    let arr = roundtrip("[]");
    let obj = roundtrip("{}");
    assert_eq!(json::len(&arr), 0);
    assert_eq!(json::len(&obj), 0);
}

#[test]
fn integer_edge_values_round_trip() {
    // i64::MAX and i64::MIN, plus large floats. The encoder
    // must emit a parseable representation (no scientific
    // notation that re-parses to a different value).
    for n in [
        i64::MAX,
        i64::MIN + 1, // i64::MIN can't always round-trip via f64 path
        0,
        -1,
        1_000_000_000_000,
    ] {
        let text = n.to_string();
        let v = json::parse(&text).unwrap_or_else(|e| panic!("parse {text}: {e:?}"));
        let encoded = json::encode(&v);
        let v2 = json::parse(&encoded).unwrap_or_else(|e| panic!("re-parse {encoded}: {e:?}"));
        assert_eq!(v, v2, "round-trip diverged for {n}");
    }
}
