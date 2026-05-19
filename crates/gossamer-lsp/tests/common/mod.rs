//! Shared helpers for the LSP integration tests.
//!
//! Wraps `gossamer_lsp::testing::ServerHandle` with a few small
//! conveniences (server-with-content factory, value-walking
//! helpers) so the per-surface test files stay focused on the
//! assertion shapes they care about.

#![allow(dead_code, unreachable_pub)]

use gossamer_lsp::testing::ServerHandle;
use gossamer_std::json::Value;

/// Spawns a fresh server with a single document open.
pub fn server_with(uri: &str, source: &str) -> ServerHandle {
    let mut server = ServerHandle::new();
    server.update(uri, source);
    server
}

/// Returns the named field on a JSON object, or `Value::Null`.
pub fn field<'v>(value: &'v Value, key: &str) -> &'v Value {
    match value {
        Value::Object(map) => map.get(key).unwrap_or(&Value::Null),
        _ => &Value::Null,
    }
}

/// Returns the string value of a JSON field, or `None`.
pub fn field_str<'v>(value: &'v Value, key: &str) -> Option<&'v str> {
    match field(value, key) {
        Value::String(s) => Some(s),
        _ => None,
    }
}

/// Returns the numeric value of a JSON field, coerced to `f64`,
/// or `None` if absent or non-numeric.
pub fn field_f64(value: &Value, key: &str) -> Option<f64> {
    match field(value, key) {
        Value::Number(n) => Some(*n),
        _ => None,
    }
}

/// Returns the integer value of a JSON field, or `None`.
pub fn field_i64(value: &Value, key: &str) -> Option<i64> {
    field_f64(value, key).map(|n| n as i64)
}

/// Returns the array under `key`, or an empty slice if missing.
pub fn field_array<'v>(value: &'v Value, key: &str) -> &'v [Value] {
    match field(value, key) {
        Value::Array(items) => items.as_slice(),
        _ => &[],
    }
}

/// Returns every `label` string in a completion-list response.
pub fn completion_labels(response: &Value) -> Vec<String> {
    let items = match response {
        Value::Array(items) => items.as_slice(),
        Value::Object(map) => match map.get("items") {
            Some(Value::Array(items)) => items.as_slice(),
            _ => &[],
        },
        _ => &[],
    };
    items
        .iter()
        .filter_map(|item| field_str(item, "label").map(str::to_string))
        .collect()
}

/// Returns the `value` field from a markup-content hover response.
pub fn hover_text(response: &Value) -> String {
    let contents = field(response, "contents");
    match contents {
        Value::Object(_) => field_str(contents, "value").unwrap_or("").to_string(),
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

/// Returns the `code` field from a diagnostic value as a string.
pub fn diagnostic_code(diag: &Value) -> Option<String> {
    match field(diag, "code") {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Returns the `message` field from a diagnostic value.
pub fn diagnostic_message(diag: &Value) -> Option<String> {
    field_str(diag, "message").map(str::to_string)
}

/// Extracts the diagnostic array from a `publishDiagnostics`
/// notification (the only one published by `ServerState::update`).
pub fn diagnostics_from(notifications: &[Value]) -> Vec<Value> {
    for notif in notifications {
        if field_str(notif, "method") != Some("textDocument/publishDiagnostics") {
            continue;
        }
        let params = field(notif, "params");
        if let Value::Array(items) = field(params, "diagnostics") {
            return items.clone();
        }
    }
    Vec::new()
}
