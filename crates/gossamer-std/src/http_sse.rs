//! Server-Sent Events (SSE) — `text/event-stream`.
//!
//! `SseStream` wraps a writer (typically the upgraded TCP
//! connection from an HTTP handler) and emits events in the
//! SSE wire format per the W3C spec:
//!
//! ```text
//!   event: <event-name>\n
//!   id: <id>\n
//!   data: <line>\n
//!   data: <line>\n
//!   \n
//! ```
//!
//! The stream sets `Content-Type: text/event-stream` and
//! `Cache-Control: no-cache` on the underlying response (callers
//! build the response themselves; helpers below populate the
//! headers).

use std::io::{self, Write};

use crate::http::{Headers, Response, StatusCode};

/// Stream wrapper that emits SSE-framed events.
pub struct SseStream<W: Write> {
    writer: W,
}

impl<W: Write> SseStream<W> {
    /// Wraps `writer`. The caller is responsible for having
    /// already written the HTTP response head with the
    /// appropriate `Content-Type` (see [`event_stream_headers`]).
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    /// Sends one event. `event` is the optional event name (or
    /// `None` to use the default `message` event), `data` is the
    /// payload (split into multiple `data:` lines on `\n`), and
    /// `id` is an optional event id used by the client for
    /// `Last-Event-Id` on reconnect.
    pub fn send(&mut self, event: Option<&str>, data: &str, id: Option<&str>) -> io::Result<()> {
        let mut out = String::with_capacity(data.len() + 32);
        if let Some(name) = event {
            out.push_str("event: ");
            out.push_str(name);
            out.push('\n');
        }
        if let Some(i) = id {
            out.push_str("id: ");
            out.push_str(i);
            out.push('\n');
        }
        for line in data.split('\n') {
            out.push_str("data: ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
        self.writer.write_all(out.as_bytes())?;
        self.writer.flush()
    }

    /// Sends a retry directive (in ms). The client uses this to
    /// govern reconnect timing.
    pub fn send_retry(&mut self, millis: u64) -> io::Result<()> {
        self.writer
            .write_all(format!("retry: {millis}\n\n").as_bytes())?;
        self.writer.flush()
    }

    /// Sends a comment line (used as a heartbeat to keep
    /// intermediaries from idling the connection).
    pub fn send_comment(&mut self, comment: &str) -> io::Result<()> {
        let mut out = String::with_capacity(comment.len() + 4);
        for line in comment.split('\n') {
            out.push(':');
            out.push(' ');
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
        self.writer.write_all(out.as_bytes())?;
        self.writer.flush()
    }
}

/// Builds the canonical headers for an `text/event-stream`
/// response. Handlers attach these to a [`Response`] before
/// hijacking the connection.
#[must_use]
pub fn event_stream_headers() -> Headers {
    let mut h = Headers::new();
    h.insert("content-type", "text/event-stream; charset=utf-8");
    h.insert("cache-control", "no-cache");
    h.insert("connection", "keep-alive");
    // Streaming responses need chunked encoding so the writer
    // can push events without knowing total length in advance.
    h.insert("transfer-encoding", "chunked");
    h
}

/// Convenience: builds a [`Response`] with the SSE headers and
/// an empty body. The caller writes the response head, then
/// streams events through the upgraded connection.
#[must_use]
pub fn response_skeleton() -> Response {
    Response {
        status: StatusCode(200),
        headers: event_stream_headers(),
        body: Vec::new(),
        raw_header_pairs: Vec::new(),
        body_stream: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_default_event() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut s = SseStream::new(&mut buf);
            s.send(None, "hello", None).unwrap();
        }
        assert_eq!(String::from_utf8_lossy(&buf), "data: hello\n\n");
    }

    #[test]
    fn send_named_event_with_id() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut s = SseStream::new(&mut buf);
            s.send(Some("update"), "payload", Some("42")).unwrap();
        }
        let expected = "event: update\nid: 42\ndata: payload\n\n";
        assert_eq!(String::from_utf8_lossy(&buf), expected);
    }

    #[test]
    fn multi_line_data_splits_on_newlines() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut s = SseStream::new(&mut buf);
            s.send(None, "line one\nline two\nline three", None)
                .unwrap();
        }
        let expected = "data: line one\ndata: line two\ndata: line three\n\n";
        assert_eq!(String::from_utf8_lossy(&buf), expected);
    }

    #[test]
    fn retry_directive() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut s = SseStream::new(&mut buf);
            s.send_retry(3000).unwrap();
        }
        assert_eq!(String::from_utf8_lossy(&buf), "retry: 3000\n\n");
    }

    #[test]
    fn comment_is_heartbeat_safe() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut s = SseStream::new(&mut buf);
            s.send_comment("keep-alive").unwrap();
        }
        assert_eq!(String::from_utf8_lossy(&buf), ": keep-alive\n\n");
    }

    #[test]
    fn headers_have_event_stream_content_type() {
        let h = event_stream_headers();
        assert_eq!(
            h.get("content-type"),
            Some("text/event-stream; charset=utf-8")
        );
        assert_eq!(h.get("cache-control"), Some("no-cache"));
        assert_eq!(h.get("transfer-encoding"), Some("chunked"));
    }

    #[test]
    fn response_skeleton_is_200_with_headers() {
        let r = response_skeleton();
        assert_eq!(r.status, StatusCode(200));
        assert_eq!(
            r.headers.get("content-type"),
            Some("text/event-stream; charset=utf-8")
        );
        assert!(r.body.is_empty());
    }

    #[test]
    fn multiple_events_accumulate() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut s = SseStream::new(&mut buf);
            s.send(None, "one", None).unwrap();
            s.send(None, "two", None).unwrap();
            s.send(None, "three", None).unwrap();
        }
        let expected = "data: one\n\ndata: two\n\ndata: three\n\n";
        assert_eq!(String::from_utf8_lossy(&buf), expected);
    }
}
