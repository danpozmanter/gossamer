//! Property tests: random `Value` -> `to_raw` -> `from_raw` -> structural
//! equality. Validates that the Phase P1 value contract is a faithful
//! round-trip for every encodable variant.

use std::sync::Arc;

use gossamer_ast::Ident;
use gossamer_interp::{Channel, SmolStr, Value};

/// Manual structural equality for `Value`.  Needed because `Value`
/// does not (yet) derive `PartialEq` — function pointers, `Mutex`,
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
                    .all(|((ia, va), (ib, vb))| ia.name == ib.name && values_equal(va, vb))
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

#[test]
fn tuple_roundtrips() {
    let v = Value::Tuple(Arc::new(vec![
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
    let v = Value::variant("Some", Arc::new(vec![Value::Int(42)]));
    assert!(values_equal(&Value::from_raw(v.to_raw()), &v));
}

#[test]
fn struct_roundtrips() {
    let v = Value::struct_(
        "Point",
        Arc::new(vec![
            (Ident::new("x"), Value::Int(1)),
            (Ident::new("y"), Value::Int(2)),
        ]),
    );
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
    use gossamer_hir::{HirExpr, HirExprKind, HirLiteral};
    use gossamer_lex::Span;

    let dummy_expr = HirExpr {
        id: gossamer_hir::HirId(0),
        span: Span::new(
            {
                let mut map = gossamer_lex::SourceMap::new();
                map.add_file("t.gos", "")
            },
            0,
            0,
        ),
        ty: gossamer_types::TyCtxt::new().unit(),
        kind: HirExprKind::Literal(HirLiteral::Unit),
    };
    let closure = gossamer_interp::Closure {
        params: Vec::new(),
        body: dummy_expr,
        captures: Vec::new(),
    };
    let v = Value::Closure(Arc::new(closure));
    let decoded = Value::from_raw(v.to_raw());
    assert!(matches!(decoded, Value::Closure(_)));
}

#[test]
fn nested_aggregate_roundtrips() {
    let v = Value::Tuple(Arc::new(vec![
        Value::Array(Arc::new(vec![Value::struct_(
            "Pair",
            Arc::new(vec![
                (Ident::new("a"), Value::Int(1)),
                (Ident::new("b"), Value::Int(2)),
            ]),
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
        let v = Value::Tuple(Arc::new(vec![
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
