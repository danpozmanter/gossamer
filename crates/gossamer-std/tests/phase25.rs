//!: JSON parser + emitter.

use gossamer_std::json::{
    Value, as_i64, decode, encode, encode_pretty, from_i64, parse, to_string,
};

#[test]
fn parse_scalar_literals() {
    assert_eq!(parse("null").unwrap(), Value::Null);
    assert_eq!(parse("true").unwrap(), Value::Bool(true));
    assert_eq!(parse("false").unwrap(), Value::Bool(false));
    assert_eq!(parse("42").unwrap(), Value::Number(42.0));
    assert_eq!(parse("\"hello\"").unwrap(), Value::String("hello".into()));
}

#[test]
fn parse_array_of_mixed_primitives() {
    let v = parse("[1, true, null, \"str\"]").unwrap();
    if let Value::Array(parts) = v {
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], Value::Number(1.0));
        assert_eq!(parts[1], Value::Bool(true));
        assert_eq!(parts[2], Value::Null);
        assert_eq!(parts[3], Value::String("str".into()));
    } else {
        panic!("expected array");
    }
}

#[test]
fn parse_object_with_nested_types() {
    let v = parse(r#"{"name": "gossamer", "stars": 42, "tags": ["lang", "gc"]}"#).unwrap();
    if let Value::Object(map) = v {
        assert_eq!(map.get("name"), Some(&Value::String("gossamer".into())));
        assert_eq!(map.get("stars"), Some(&Value::Number(42.0)));
        assert!(matches!(map.get("tags"), Some(Value::Array(_))));
    } else {
        panic!("expected object");
    }
}

#[test]
fn parse_reports_line_and_column_on_error() {
    let err = parse("{\n  \"key\": oops\n}").unwrap_err();
    assert_eq!(err.line, 2);
    assert!(err.column >= 10);
    let err = parse("trailing").unwrap_err();
    assert_eq!(err.line, 1);
}

#[test]
fn encode_round_trips_through_parse() {
    let source = r#"{"a":1,"b":[2,3],"c":{"d":null,"e":true}}"#;
    let value = parse(source).unwrap();
    let encoded = encode(&value);
    // Object order is sorted by BTreeMap so output is deterministic.
    assert_eq!(encoded, source);
    let reparsed = parse(&encoded).unwrap();
    assert_eq!(reparsed, value);
}

#[test]
fn encode_escapes_control_characters_in_strings() {
    let value = Value::String("a\"\\\nb".to_string());
    let encoded = encode(&value);
    assert_eq!(encoded, r#""a\"\\\nb""#);
    let reparsed = parse(&encoded).unwrap();
    assert_eq!(reparsed, value);
}

#[test]
fn encode_pretty_indents_two_spaces() {
    let value = parse(r#"{"a":[1,2]}"#).unwrap();
    let pretty = encode_pretty(&value);
    assert!(pretty.contains("  \"a\""));
    assert!(pretty.contains("    1"));
}

#[test]
fn numeric_helpers_round_trip_exact_integers() {
    let value = from_i64(12345);
    assert_eq!(as_i64(&value), Some(12345));
    assert_eq!(encode(&value), "12345");
    let flt = Value::Number(0.5);
    assert!(as_i64(&flt).is_none());
}

#[test]
fn decode_and_to_string_are_aliases() {
    let value = decode("[1, 2]").unwrap();
    assert_eq!(to_string(&value), "[1,2]");
}

#[test]
fn empty_object_and_array_round_trip() {
    let object = parse("{}").unwrap();
    assert_eq!(encode(&object), "{}");
    let array = parse("[]").unwrap();
    assert_eq!(encode(&array), "[]");
}
