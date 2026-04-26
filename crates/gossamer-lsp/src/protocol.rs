//! Minimal LSP wire-format helpers backed by `gossamer_std::json`.
//! The full LSP spec is large; this module implements only the
//! subset Gossamer's first-slice server uses. It hand-writes the
//! `Content-Length: N\r\n\r\n` framing and goes straight to / from
//! [`gossamer_std::json::Value`] without a separate typed DTO layer.

#![forbid(unsafe_code)]

use std::io::{BufRead, Write};

use gossamer_std::json::{self, Value};

/// Wraps stdin + stdout into a framed JSON-RPC transport.
pub(crate) struct Transport<R: BufRead, W: Write> {
    reader: R,
    writer: W,
    buffer: Vec<u8>,
}

impl<R: BufRead, W: Write> Transport<R, W> {
    /// Constructs a transport bound to the supplied streams.
    pub(crate) fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            buffer: Vec::new(),
        }
    }

    /// Reads one framed message, returning `None` on clean EOF.
    pub(crate) fn read_message(&mut self) -> std::io::Result<Option<Value>> {
        let mut content_length: Option<usize> = None;
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.reader.read_line(&mut line)?;
            if n == 0 {
                return Ok(None);
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break;
            }
            if let Some(value) = trimmed.strip_prefix("Content-Length:") {
                content_length = value.trim().parse::<usize>().ok();
            }
        }
        let Some(len) = content_length else {
            return Err(std::io::Error::other(
                "LSP frame missing Content-Length header",
            ));
        };
        self.buffer.resize(len, 0);
        self.reader.read_exact(&mut self.buffer)?;
        let text = std::str::from_utf8(&self.buffer).map_err(std::io::Error::other)?;
        let value = json::parse(text).map_err(|e| std::io::Error::other(format!("{e}")))?;
        Ok(Some(value))
    }

    /// Writes one framed message.
    pub(crate) fn write_message(&mut self, value: &Value) -> std::io::Result<()> {
        let payload = json::encode(value);
        write!(self.writer, "Content-Length: {}\r\n\r\n", payload.len())?;
        self.writer.write_all(payload.as_bytes())?;
        self.writer.flush()
    }
}

/// Builds a successful JSON-RPC response.
pub(crate) fn response_ok(id: Value, result: Value) -> Value {
    let mut map = std::collections::BTreeMap::new();
    map.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
    map.insert("id".to_string(), id);
    map.insert("result".to_string(), result);
    Value::Object(map)
}

/// Builds a JSON-RPC notification (a message without an `id`).
pub(crate) fn notification(method: &str, params: Value) -> Value {
    let mut map = std::collections::BTreeMap::new();
    map.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
    map.insert("method".to_string(), Value::String(method.to_string()));
    map.insert("params".to_string(), params);
    Value::Object(map)
}

/// Extracts a field from a JSON object, returning `Value::Null` when
/// absent so callers can unify the empty-field path with the
/// Null-field path.
pub(crate) fn field<'v>(object: &'v Value, key: &str) -> &'v Value {
    if let Value::Object(map) = object {
        map.get(key).unwrap_or(&Value::Null)
    } else {
        &Value::Null
    }
}

/// Extracts a string field from a JSON object.
pub(crate) fn field_str<'v>(object: &'v Value, key: &str) -> Option<&'v str> {
    if let Value::String(s) = field(object, key) {
        Some(s.as_str())
    } else {
        None
    }
}

/// Extracts an `i64` field from a JSON object (LSP uses
/// non-negative integers for line/character positions).
pub(crate) fn field_u32(object: &Value, key: &str) -> Option<u32> {
    match field(object, key) {
        Value::Number(n) if n.is_finite() && *n >= 0.0 => {
            if n.fract() == 0.0 {
                Some(*n as u32)
            } else {
                None
            }
        }
        _ => None,
    }
}
