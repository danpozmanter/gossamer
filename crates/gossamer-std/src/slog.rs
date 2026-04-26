//! Runtime support for `std::slog` — structured, levelled logging.
//! The core type is [`Logger`]: threads key/value pairs through
//! `with`, emits records via `info`/`warn`/`error`, and delegates
//! rendering to a pluggable [`Handler`].

#![forbid(unsafe_code)]

use std::fmt::Write;
use std::io::{self, Write as IoWrite};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Severity of a log record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// Developer-facing detail, off by default.
    Debug,
    /// Informational.
    Info,
    /// Unexpected but recoverable.
    Warn,
    /// Broken state; needs attention.
    Error,
}

impl Level {
    /// Canonical short tag (`DEBUG`, `INFO`, `WARN`, `ERROR`).
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

/// One key/value pair threaded through a [`Logger`].
#[derive(Debug, Clone)]
pub struct Field {
    /// Field key.
    pub key: String,
    /// Stringified value.
    pub value: String,
}

impl Field {
    /// Key/value constructor.
    #[must_use]
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

/// Emits a rendered record.
pub trait Handler: Send + Sync {
    /// Writes `record` to the handler's sink.
    fn emit(&self, record: &Record);
    /// Minimum level this handler cares about.
    fn min_level(&self) -> Level {
        Level::Info
    }
}

/// Fully-assembled log record seen by a [`Handler`].
#[derive(Debug, Clone)]
pub struct Record {
    /// Severity.
    pub level: Level,
    /// Time the record was produced.
    pub time: SystemTime,
    /// Primary message.
    pub message: String,
    /// Structured fields.
    pub fields: Vec<Field>,
}

/// Logger handle — cheap to clone.
#[derive(Clone)]
pub struct Logger {
    handler: Arc<dyn Handler>,
    fields: Vec<Field>,
}

impl Logger {
    /// Builds a logger over a handler.
    #[must_use]
    pub fn new(handler: Arc<dyn Handler>) -> Self {
        Self {
            handler,
            fields: Vec::new(),
        }
    }

    /// Returns a clone that always carries the supplied field.
    #[must_use]
    pub fn with(&self, field: Field) -> Self {
        let mut fields = self.fields.clone();
        fields.push(field);
        Self {
            handler: Arc::clone(&self.handler),
            fields,
        }
    }

    /// Logs at [`Level::Info`].
    pub fn info(&self, message: impl Into<String>, extra: impl IntoIterator<Item = Field>) {
        self.emit(Level::Info, message, extra);
    }

    /// Logs at [`Level::Warn`].
    pub fn warn(&self, message: impl Into<String>, extra: impl IntoIterator<Item = Field>) {
        self.emit(Level::Warn, message, extra);
    }

    /// Logs at [`Level::Error`].
    pub fn error(&self, message: impl Into<String>, extra: impl IntoIterator<Item = Field>) {
        self.emit(Level::Error, message, extra);
    }

    /// Logs at [`Level::Debug`].
    pub fn debug(&self, message: impl Into<String>, extra: impl IntoIterator<Item = Field>) {
        self.emit(Level::Debug, message, extra);
    }

    fn emit(
        &self,
        level: Level,
        message: impl Into<String>,
        extra: impl IntoIterator<Item = Field>,
    ) {
        if level < self.handler.min_level() {
            return;
        }
        let mut fields = self.fields.clone();
        fields.extend(extra);
        let record = Record {
            level,
            time: SystemTime::now(),
            message: message.into(),
            fields,
        };
        self.handler.emit(&record);
    }
}

/// Writes records as `LEVEL message key=value key=value` lines.
pub struct TextHandler {
    writer: Mutex<Box<dyn IoWrite + Send>>,
    min_level: Level,
}

impl TextHandler {
    /// Wraps `writer` with a minimum level.
    pub fn new<W: IoWrite + Send + 'static>(writer: W, min_level: Level) -> Self {
        Self {
            writer: Mutex::new(Box::new(writer)),
            min_level,
        }
    }
}

impl Handler for TextHandler {
    fn emit(&self, record: &Record) {
        let mut line = format!("{} {}", record.level.tag(), record.message);
        for field in &record.fields {
            let _ = write!(line, " {}={}", field.key, field.value);
        }
        line.push('\n');
        let mut sink = self.writer.lock().unwrap();
        let _ = sink.write_all(line.as_bytes());
    }

    fn min_level(&self) -> Level {
        self.min_level
    }
}

/// Writes records as `{"level":..., "msg":..., "fields":{...}}`
/// JSON-shaped lines.
pub struct JsonHandler {
    writer: Mutex<Box<dyn IoWrite + Send>>,
    min_level: Level,
}

impl JsonHandler {
    /// Wraps `writer` with a minimum level.
    pub fn new<W: IoWrite + Send + 'static>(writer: W, min_level: Level) -> Self {
        Self {
            writer: Mutex::new(Box::new(writer)),
            min_level,
        }
    }
}

impl Handler for JsonHandler {
    fn emit(&self, record: &Record) {
        let mut line = String::new();
        line.push('{');
        let _ = write!(line, "\"level\":\"{}\"", record.level.tag());
        let _ = write!(
            line,
            ",\"msg\":{}",
            json_string(&record.message)
        );
        for field in &record.fields {
            let _ = write!(
                line,
                ",{}:{}",
                json_string(&field.key),
                json_string(&field.value)
            );
        }
        line.push('}');
        line.push('\n');
        let mut sink = self.writer.lock().unwrap();
        let _ = sink.write_all(line.as_bytes());
    }

    fn min_level(&self) -> Level {
        self.min_level
    }
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Convenience constructor for a stderr text logger at `min_level`.
#[must_use]
pub fn stderr_text(min_level: Level) -> Logger {
    Logger::new(Arc::new(TextHandler::new(io::stderr(), min_level)))
}

/// Convenience constructor for a stderr JSON-lines logger at
/// `min_level`. Mirrors [`stderr_text`] but produces one JSON
/// object per record, matching `log/slog`'s `NewJSONHandler`
/// shape from Go.
#[must_use]
pub fn stderr_json(min_level: Level) -> Logger {
    Logger::new(Arc::new(JsonHandler::new(io::stderr(), min_level)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Capture {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl IoWrite for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.buf.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn handler_captures_text(min: Level) -> (Logger, Arc<Mutex<Vec<u8>>>) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let capture = Capture { buf: Arc::clone(&buf) };
        (Logger::new(Arc::new(TextHandler::new(capture, min))), buf)
    }

    #[test]
    fn info_renders_text() {
        let (logger, buf) = handler_captures_text(Level::Info);
        logger.info("hi", [Field::new("user", "jane")]);
        let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(out.contains("INFO hi user=jane"), "was: {out}");
    }

    #[test]
    fn level_below_min_is_suppressed() {
        let (logger, buf) = handler_captures_text(Level::Warn);
        logger.info("ignored", []);
        assert!(buf.lock().unwrap().is_empty());
    }

    #[test]
    fn with_threads_fields_through_children() {
        let (root, buf) = handler_captures_text(Level::Info);
        let child = root.with(Field::new("req", "42"));
        child.warn("late", [Field::new("ms", "120")]);
        let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(out.contains("req=42"));
        assert!(out.contains("ms=120"));
    }

    #[test]
    fn json_handler_escapes_special_chars() {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let capture = Capture { buf: Arc::clone(&buf) };
        let logger = Logger::new(Arc::new(JsonHandler::new(capture, Level::Info)));
        logger.info("quote: \"x\"", []);
        let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(out.contains(r#""msg":"quote: \"x\"""#), "was: {out}");
    }
}
