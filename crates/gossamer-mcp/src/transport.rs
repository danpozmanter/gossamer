//! Newline-delimited JSON-RPC framing for the MCP stdio transport.
//! One message per line - no `Content-Length` headers (that is LSP
//! framing). stdout carries protocol messages only.

use std::io::{BufRead, Write};

use gossamer_std::json::{self, Value};

/// One read attempt from the client stream.
pub(crate) enum Incoming {
    /// A parsed JSON-RPC message.
    Message(Value),
    /// A non-empty line that failed to parse as JSON.
    ParseError,
    /// Clean end of input.
    Eof,
}

/// Wraps the client streams into a line-framed JSON-RPC transport.
pub(crate) struct Transport<R: BufRead, W: Write> {
    reader: R,
    writer: W,
}

impl<R: BufRead, W: Write> Transport<R, W> {
    /// Constructs a transport bound to the supplied streams.
    pub(crate) fn new(reader: R, writer: W) -> Self {
        Self { reader, writer }
    }

    /// Reads the next message, skipping blank lines.
    pub(crate) fn read_message(&mut self) -> std::io::Result<Incoming> {
        let mut line = String::new();
        loop {
            line.clear();
            if self.reader.read_line(&mut line)? == 0 {
                return Ok(Incoming::Eof);
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            return Ok(match json::parse(trimmed) {
                Ok(value) => Incoming::Message(value),
                Err(_) => Incoming::ParseError,
            });
        }
    }

    /// Writes one message as a single line.
    pub(crate) fn write_message(&mut self, value: &Value) -> std::io::Result<()> {
        let payload = json::encode(&wire_integers(value));
        self.writer.write_all(payload.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()
    }
}

/// Rewrites every integral `Value::Number` in `value` to `Value::Int`.
///
/// JSON-RPC ids and MCP numeric fields are integers on the wire.
/// `gossamer_std::json` stores numbers as `Value::Number(f64)`, whose
/// encoder renders an integral float as `3.0`; a conformant client
/// rejects that shape for an echoed request id. Same adaptation as the
/// LSP transport applies at its serialization boundary.
fn wire_integers(value: &Value) -> Value {
    match value {
        Value::Number(n)
            if n.is_finite()
                && n.fract() == 0.0
                && *n >= i64::MIN as f64
                && *n <= i64::MAX as f64 =>
        {
            Value::Int(*n as i64)
        }
        Value::Array(items) => Value::Array(items.iter().map(wire_integers).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), wire_integers(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}
