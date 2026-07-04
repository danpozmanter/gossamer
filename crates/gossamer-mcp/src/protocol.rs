//! JSON-RPC envelope helpers backed by `gossamer_std::json::Value`.

use std::collections::BTreeMap;

use gossamer_std::json::{self, Value};

static NULL: Value = Value::Null;

/// Extracts an object field, unifying "absent" with `Value::Null`.
pub(crate) fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    json::get(value, key).unwrap_or(&NULL)
}

/// Extracts a string-typed object field.
pub(crate) fn field_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    json::as_str(field(value, key))
}

/// Builds an object from static keys; the workhorse for result shapes.
pub(crate) fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(
        pairs
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect::<BTreeMap<_, _>>(),
    )
}

/// Shorthand string value.
pub(crate) fn s(text: &str) -> Value {
    Value::String(text.to_string())
}

/// Builds a successful JSON-RPC response.
pub(crate) fn response_ok(id: Value, result: Value) -> Value {
    obj(vec![("jsonrpc", s("2.0")), ("id", id), ("result", result)])
}

/// Builds a JSON-RPC error response.
pub(crate) fn response_err(id: Value, code: i64, message: &str) -> Value {
    let error = obj(vec![("code", Value::Int(code)), ("message", s(message))]);
    obj(vec![("jsonrpc", s("2.0")), ("id", id), ("error", error)])
}
