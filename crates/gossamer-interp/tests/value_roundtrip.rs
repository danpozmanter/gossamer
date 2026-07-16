//! Property tests: random `Value` -> `to_raw` -> `from_raw` -> structural
//! equality. Validates that the Phase P1 value contract is a faithful
//! round-trip for every encodable variant.

use std::sync::Arc;

use gossamer_interp::{Channel, SmolStr, Value};

/// Manual structural equality for `Value`.  Needed because `Value`
/// does not (yet) derive `PartialEq` - function pointers, `Mutex`,
/// and HIR types prevent a blanket derive.
fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Unit, Value::Unit) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
        (Value::Char(a), Value::Char(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Tuple(a), Value::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| values_equal(x, y))
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| values_equal(x, y))
        }
        (Value::Variant(a), Value::Variant(b)) => {
            a.name == b.name
                && a.fields.len() == b.fields.len()
                && a.fields
                    .iter()
                    .zip(b.fields.iter())
                    .all(|(x, y)| values_equal(x, y))
        }
        (Value::Struct(a), Value::Struct(b)) => {
            a.name == b.name
                && a.fields.len() == b.fields.len()
                && a.fields
                    .iter()
                    .zip(b.fields.iter())
                    .all(|((ia, va), (ib, vb))| ia == ib && values_equal(va, vb))
        }
        (Value::Channel(_), Value::Channel(_)) => {
            // Channels have no structural equality; roundtrip is
            // verified by variant match only.
            true
        }
        (Value::Closure(_), Value::Closure(_)) => {
            // Closures compare by pointer; roundtrip is verified by
            // variant match only.
            true
        }
        (Value::Void, Value::Void) => true,
        _ => false,
    }
}

#[test]
fn unit_roundtrips() {
    let v = Value::Unit;
    assert!(values_equal(&Value::from_raw(v.to_raw()), &v));
}

#[test]
fn bool_roundtrips() {
    for b in [false, true] {
        let v = Value::Bool(b);
        assert!(values_equal(&Value::from_raw(v.to_raw()), &v), "bool {b}");
    }
}

#[test]
fn small_int_roundtrips() {
    for n in [0i64, 1, -1, 42, i64::from(i32::MAX), i64::from(i32::MIN)] {
        let v = Value::Int(n);
        assert!(values_equal(&Value::from_raw(v.to_raw()), &v), "int {n}");
    }
}

#[test]
fn float_roundtrips() {
    for f in [0.0f64, -0.0, 1.0, -1.5, 2.0, 4.0] {
        let v = Value::Float(f);
        assert!(values_equal(&Value::from_raw(v.to_raw()), &v), "float {f}");
    }
}

#[test]
fn char_roundtrips() {
    for c in ['a', ' ', '\n', 'ñ', '中', '\0'] {
        let v = Value::Char(c);
        assert!(values_equal(&Value::from_raw(v.to_raw()), &v), "char {c:?}");
    }
}

#[test]
fn string_roundtrips() {
    for text in ["", "hello", "hello, world", "unicode: ñ 中 🎉"] {
        let v = Value::String(SmolStr::from(text.to_string()));
        assert!(
            values_equal(&Value::from_raw(v.to_raw()), &v),
            "string {text:?}"
        );
    }
}

/// Heap-backed strings and typed numeric storage cross `GossamerValue` as
/// shared VM handles. This guards against accidentally rebuilding their
/// buffers while marshalling a JIT `Value` argument or return.
#[test]
fn raw_heap_handles_preserve_vm_backing_storage() {
    let string = Value::String(SmolStr::from(
        "a string longer than seven bytes".to_string(),
    ));
    let Value::String(before_string) = &string else {
        unreachable!()
    };
    let decoded_string = Value::from_raw(string.to_raw());
    let Value::String(after_string) = decoded_string else {
        panic!("string raw handle must decode as String")
    };
    assert_eq!(
        before_string.as_str().as_ptr(),
        after_string.as_str().as_ptr()
    );

    let ints = Arc::new(vec![1_i64, 2, 3]);
    let decoded_ints = Value::from_raw(Value::IntArray(Arc::clone(&ints)).to_raw());
    let Value::IntArray(after_ints) = decoded_ints else {
        panic!("typed i64 raw handle must retain typed storage")
    };
    assert!(Arc::ptr_eq(&ints, &after_ints));

    let floats = Arc::new(vec![1.0_f64, 2.0, 3.0]);
    let decoded_floats = Value::from_raw(Value::FloatVec(Arc::clone(&floats)).to_raw());
    let Value::FloatVec(after_floats) = decoded_floats else {
        panic!("typed f64 raw handle must retain typed storage")
    };
    assert!(Arc::ptr_eq(&floats, &after_floats));
}

#[test]
fn smolstr_uppercase_fast_path_matches_unicode_contract() {
    assert_eq!(
        SmolStr::to_uppercase_from("json-tag_42").as_str(),
        "JSON-TAG_42"
    );
    assert_eq!(
        SmolStr::to_uppercase_from("longer-json-tag_42").as_str(),
        "LONGER-JSON-TAG_42"
    );
    assert_eq!(SmolStr::to_uppercase_from("straße").as_str(), "STRASSE");
}

#[test]
fn tuple_roundtrips() {
    let v = Value::Tuple(Arc::from(vec![
        Value::Int(1),
        Value::Bool(false),
        Value::String(SmolStr::from("x".to_string())),
    ]));
    assert!(values_equal(&Value::from_raw(v.to_raw()), &v));
}

#[test]
fn array_roundtrips() {
    let v = Value::Array(Arc::new(vec![
        Value::Int(10),
        Value::Int(20),
        Value::Int(30),
    ]));
    assert!(values_equal(&Value::from_raw(v.to_raw()), &v));
}

#[test]
fn variant_roundtrips() {
    let v = Value::variant("Some", vec![Value::Int(42)]);
    assert!(values_equal(&Value::from_raw(v.to_raw()), &v));
}

#[test]
fn struct_roundtrips() {
    let v = Value::struct_("Point", vec![("x", Value::Int(1)), ("y", Value::Int(2))]);
    assert!(values_equal(&Value::from_raw(v.to_raw()), &v));
}

#[test]
fn channel_roundtrips() {
    let v = Value::Channel(Channel::new());
    let decoded = Value::from_raw(v.to_raw());
    assert!(matches!(decoded, Value::Channel(_)));
}

#[test]
fn closure_roundtrips() {
    let closure = gossamer_interp::Closure {
        chunk: gossamer_interp::FnChunk::default().into_shared(),
        capture_values: Vec::new(),
    };
    let v = Value::Closure(Arc::new(closure));
    let decoded = Value::from_raw(v.to_raw());
    assert!(matches!(decoded, Value::Closure(_)));
}

#[test]
fn nested_aggregate_roundtrips() {
    let v = Value::Tuple(Arc::from(vec![
        Value::Array(Arc::new(vec![Value::struct_(
            "Pair",
            vec![("a", Value::Int(1)), ("b", Value::Int(2))],
        )])),
        Value::Bool(true),
    ]));
    assert!(values_equal(&Value::from_raw(v.to_raw()), &v));
}

#[test]
fn builtin_maps_to_sentinel_and_back_to_void() {
    let v = Value::builtin("println", |_args| Ok(Value::Unit));
    let decoded = Value::from_raw(v.to_raw());
    assert!(matches!(decoded, Value::Unit | Value::Void));
}

#[test]
fn void_roundtrips_as_sentinel() {
    let v = Value::Void;
    let decoded = Value::from_raw(v.to_raw());
    assert!(matches!(decoded, Value::Unit | Value::Void));
}

/// Regression: the heap registry used to leak monotonically because
/// `lookup_heap` cloned without freeing the slot. Each `to_raw` →
/// `from_raw` cycle is now a balanced register/take pair, so the
/// registry stays bounded by the in-flight raw-value count.
#[test]
fn heap_registry_stays_bounded_under_repeated_roundtrip() {
    use gossamer_interp::registry_stats_for_test;

    let (baseline_slots, _) = registry_stats_for_test();
    for _ in 0..10_000 {
        let v = Value::Tuple(Arc::from(vec![
            Value::Int(1),
            Value::String(SmolStr::from("hello".to_string())),
            Value::Bool(true),
        ]));
        let raw = v.to_raw();
        let _decoded = Value::from_raw(raw);
    }
    let (final_slots, occupied) = registry_stats_for_test();
    let growth = final_slots.saturating_sub(baseline_slots);
    assert!(
        growth < 64,
        "registry grew by {growth} slots over 10000 round-trips (before fix this was 10000)"
    );
    assert_eq!(
        occupied, 0,
        "every round-tripped slot should have been taken; {occupied} slots still occupied"
    );
}
