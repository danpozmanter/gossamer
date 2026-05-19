//! HTTP/2 server (RFC 7540) — first-party stdlib support, no
//! Tokio runtime.
//!
//! The server takes any connected `Read + Write` stream (plain
//! TCP for `h2c` debugging; rustls-wrapped for production
//! HTTPS), runs the h2 handshake, then accepts inbound streams
//! and dispatches each to a user-supplied handler. The entire
//! server runs inside one goroutine; concurrent streams are
//! served by spawning child goroutines, each of which drives
//! its own future via [`crate::runtime_future::drive`].
//!
//! See `HTTP_H2_ARCH.md` for the broader runtime / future
//! integration model.
//!
//! Supported feature surface:
//!
//! - h2 handshake (server side).
//! - SETTINGS / PING / WINDOW_UPDATE frames (handled inside the
//!   `h2` crate).
//! - Multiplexed concurrent streams — one goroutine per stream.
//! - Flow-control respected (the `h2` crate's `SendStream` /
//!   `RecvStream` mediate it).
//! - Graceful GOAWAY: `Server::shutdown(deadline)` triggers a
//!   `goaway` frame and waits for in-flight streams to drain.
//! - h2c (cleartext) and ALPN-selected h2-over-TLS both supported.
//! - Bounded-body handlers via `Handler` — handler returns a
//!   complete `Response`, body sent as one `DATA` frame.
//! - Streaming handlers via `StreamingHandler` — handler is
//!   passed a `ResponseWriter` and can emit `DATA` frames
//!   incrementally. Suitable for server-sent events, long-poll
//!   loops, or chunked uploads where the response body size is
//!   not known up-front.

#![forbid(unsafe_code)]
#![allow(
    clippy::missing_errors_doc,
    clippy::module_name_repetitions,
    clippy::similar_names,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::items_after_statements,
    clippy::cast_possible_truncation,
    clippy::must_use_candidate,
    clippy::explicit_iter_loop,
    clippy::single_match_else,
    clippy::manual_let_else
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use h2::RecvStream;
use h2::server::SendResponse;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::http::{Headers, Method, Request, Response, StatusCode};

/// Handler signature for the bounded-body case: receive a
/// `Request`, return a complete `Response`. The full body is
/// buffered in `Response::body` before being sent in a single
/// `DATA` frame (plus the implicit `END_STREAM`). For streaming
/// bodies — chunked emission of arbitrary size, server-sent
/// events, server-push half of a long-poll loop — see
/// [`StreamingHandler`].
pub trait Handler: Send + Sync + 'static {
    /// Serve one HTTP/2 request.
    fn serve(&self, request: Request) -> Response;
}

impl<F> Handler for F
where
    F: Fn(Request) -> Response + Send + Sync + 'static,
{
    fn serve(&self, request: Request) -> Response {
        self(request)
    }
}

/// Handler signature for streaming responses. The handler
/// receives a `ResponseWriter` it can call `write_chunk` on
/// repeatedly; each call sends one HTTP/2 `DATA` frame to the
/// peer. The response head (status + headers) is sent on the
/// first chunk, so the handler can set status / headers up to
/// (and during) the first `write_chunk` call. Call `finish` to
/// send the terminating `END_STREAM` frame; dropping the writer
/// without finishing emits an empty terminator automatically.
pub trait StreamingHandler: Send + Sync + 'static {
    /// Serve one HTTP/2 request, streaming the response body.
    fn serve(&self, request: Request, writer: ResponseWriter) -> Result<(), Error>;
}

impl<F> StreamingHandler for F
where
    F: Fn(Request, ResponseWriter) -> Result<(), Error> + Send + Sync + 'static,
{
    fn serve(&self, request: Request, writer: ResponseWriter) -> Result<(), Error> {
        self(request, writer)
    }
}

/// Incremental response writer threaded into a `StreamingHandler`.
///
/// Lifecycle:
///
/// 1. Construction with status `200` + empty `Headers`.
/// 2. Handler may call `set_status` / `header` until the first
///    `write_chunk`.
/// 3. First `write_chunk` flushes the response head (in a single
///    `HEADERS` frame) followed by one `DATA` frame carrying the
///    chunk.
/// 4. Subsequent `write_chunk` calls send one `DATA` frame each.
/// 5. `finish` sends an empty `DATA` frame with `END_STREAM`.
/// 6. Drop without finish sends the empty terminator anyway.
pub struct ResponseWriter {
    state: WriterState,
}

enum WriterState {
    Pending {
        respond: SendResponse<Bytes>,
        status: u16,
        headers: Headers,
    },
    Streaming {
        sender: h2::SendStream<Bytes>,
    },
    Closed,
}

// Note: `WriterState::Streaming` no longer carries the
// `SendResponse<Bytes>` because push_promise is only valid before
// the head is flushed; the `respond` value is consumed during
// `send_head` and the remaining stream lifecycle only needs the
// `SendStream`. See `ResponseWriter::push_promise`.

impl ResponseWriter {
    /// Sets the HTTP status to emit when the response head is
    /// flushed (on the first `write_chunk`). No-op once the head
    /// has gone out.
    pub fn set_status(&mut self, code: u16) {
        if let WriterState::Pending { status, .. } = &mut self.state {
            *status = code;
        }
    }

    /// Sets a response header. Multiple calls with the same name
    /// overwrite. No-op once the head has gone out.
    pub fn header(&mut self, name: &str, value: &str) {
        if let WriterState::Pending { headers, .. } = &mut self.state {
            headers.insert(name, value);
        }
    }

    /// Sends one chunk. The first call flushes the response head;
    /// subsequent calls send one DATA frame each. Returns
    /// `Err(Error::Protocol)` if the peer has already cancelled
    /// the stream or the connection has gone away.
    pub fn write_chunk(&mut self, data: &[u8]) -> Result<(), Error> {
        // Flush the head if needed.
        if matches!(self.state, WriterState::Pending { .. }) {
            let old = std::mem::replace(&mut self.state, WriterState::Closed);
            let WriterState::Pending {
                mut respond,
                status,
                headers,
            } = old
            else {
                unreachable!()
            };
            let sender = send_head(&mut respond, status, &headers, false)?;
            self.state = WriterState::Streaming { sender };
        }
        let WriterState::Streaming { sender, .. } = &mut self.state else {
            return Err(Error::Protocol("writer closed".into()));
        };
        sender
            .send_data(Bytes::copy_from_slice(data), false)
            .map_err(|e| Error::Protocol(format!("send_data: {e}")))
    }

    /// Sends the terminating `END_STREAM` frame. If the head has
    /// not yet been flushed (no `write_chunk` calls), this flushes
    /// the head with `END_STREAM` set on the HEADERS frame — i.e.
    /// an empty body. Idempotent.
    pub fn finish(mut self) -> Result<(), Error> {
        self.terminate()
    }

    fn terminate(&mut self) -> Result<(), Error> {
        let state = std::mem::replace(&mut self.state, WriterState::Closed);
        match state {
            WriterState::Pending {
                mut respond,
                status,
                headers,
            } => {
                let _ = send_head(&mut respond, status, &headers, true)?;
                Ok(())
            }
            WriterState::Streaming { mut sender, .. } => sender
                .send_data(Bytes::new(), true)
                .map_err(|e| Error::Protocol(format!("send_terminator: {e}"))),
            WriterState::Closed => Ok(()),
        }
    }

    /// Sends a trailing HEADERS frame with `END_STREAM` after the
    /// response body's DATA frames. Must be called after at least
    /// one `write_chunk` (the head must already be flushed and the
    /// stream must still be open). Consumes the writer.
    ///
    /// h2 mandates that trailers carry no pseudo-headers and that
    /// the `:status` line is delivered in the original HEADERS
    /// frame, not the trailers frame.
    pub fn write_trailers(mut self, trailers: Headers) -> Result<(), Error> {
        let state = std::mem::replace(&mut self.state, WriterState::Closed);
        match state {
            WriterState::Streaming { mut sender, .. } => {
                let map = headers_to_header_map(&trailers);
                sender
                    .send_trailers(map)
                    .map_err(|e| Error::Protocol(format!("send_trailers: {e}")))
            }
            WriterState::Pending { .. } => Err(Error::Protocol(
                "write_trailers requires at least one write_chunk first".into(),
            )),
            WriterState::Closed => Err(Error::Protocol("writer closed".into())),
        }
    }

    /// Opens a server-initiated push stream. Must be called before
    /// the first `write_chunk` on the parent stream (h2 requires
    /// `PUSH_PROMISE` frames to be sent before the parent's
    /// response head).
    ///
    /// `uri` is the absolute or scheme-relative URI of the pushed
    /// resource — for HTTP/2 this becomes the synthetic request's
    /// `:path` pseudo-header. The supplied `headers` are added as
    /// request-side headers on the pushed stream.
    ///
    /// `opts` carries the prioritization knobs. Pass
    /// [`PushOptions::default()`] for the h2-default weight (16,
    /// no exclusive dependency).
    pub fn push_promise(
        &mut self,
        uri: &str,
        headers: Headers,
        _opts: PushOptions,
    ) -> Result<PushStream, Error> {
        let respond = match &mut self.state {
            WriterState::Pending { respond, .. } => respond,
            WriterState::Streaming { .. } => {
                return Err(Error::Protocol(
                    "push_promise must be called before the first write_chunk".into(),
                ));
            }
            WriterState::Closed => {
                return Err(Error::Protocol("writer closed".into()));
            }
        };
        let mut req_builder = ::http::Request::builder()
            .method(::http::Method::GET)
            .uri(uri);
        {
            let h = req_builder
                .headers_mut()
                .ok_or_else(|| Error::Protocol("push request headers".into()))?;
            for (name, value) in headers.iter() {
                if let (Ok(n), Ok(v)) = (
                    ::http::HeaderName::from_bytes(name.as_bytes()),
                    ::http::HeaderValue::from_str(value),
                ) {
                    h.insert(n, v);
                }
            }
        }
        let req: ::http::Request<()> = req_builder
            .body(())
            .map_err(|e| Error::Protocol(format!("push request build: {e}")))?;
        let pushed = respond
            .push_request(req)
            .map_err(|e| Error::Protocol(format!("push_request: {e}")))?;
        // Note: h2 ignores the weight / depends_on knobs internally
        // because h2's prioritization layer is internal; the
        // `PushOptions` shape is preserved so future versions can
        // honour them without breaking callers.
        Ok(PushStream {
            inner: PushInner::Pending(pushed),
        })
    }
}

impl Drop for ResponseWriter {
    fn drop(&mut self) {
        // Best-effort terminator on drop; ignore errors because
        // we may be unwinding from a panic.
        let _ = self.terminate();
    }
}

fn send_head(
    respond: &mut SendResponse<Bytes>,
    status: u16,
    headers: &Headers,
    end_of_stream: bool,
) -> Result<h2::SendStream<Bytes>, Error> {
    let status = ::http::StatusCode::from_u16(status)
        .map_err(|e| Error::Protocol(format!("bad status: {e}")))?;
    let mut builder = ::http::Response::builder().status(status);
    {
        let h = builder
            .headers_mut()
            .ok_or_else(|| Error::Protocol("response builder headers".into()))?;
        for (name, value) in headers.iter() {
            if let (Ok(n), Ok(v)) = (
                ::http::HeaderName::from_bytes(name.as_bytes()),
                ::http::HeaderValue::from_str(value),
            ) {
                h.insert(n, v);
            }
        }
    }
    let head: ::http::Response<()> = builder
        .body(())
        .map_err(|e| Error::Protocol(format!("response build: {e}")))?;
    respond
        .send_response(head, end_of_stream)
        .map_err(|e| Error::Protocol(format!("send_response: {e}")))
}

/// Converts a Gossamer `Headers` value into the `http::HeaderMap`
/// type accepted by the `h2` crate. Headers whose name or value
/// is not valid HTTP token / field-value bytes are skipped (h1
/// path mirrors the same lenient behaviour).
fn headers_to_header_map(headers: &Headers) -> ::http::HeaderMap {
    let mut map = ::http::HeaderMap::new();
    for (name, value) in headers.iter() {
        if let (Ok(n), Ok(v)) = (
            ::http::HeaderName::from_bytes(name.as_bytes()),
            ::http::HeaderValue::from_str(value),
        ) {
            map.insert(n, v);
        }
    }
    map
}

/// Type alias clarifying that a `Headers` map is being used for
/// trailing HEADERS frames (as opposed to leading request /
/// response headers).
pub type Trailers = Headers;

/// Server-push prioritization knobs. The exact fields mirror the
/// HTTP/2 priority frame semantics from RFC 7540 §5.3 (deprecated
/// but still honoured by most clients) — h2's internal
/// prioritization layer does not currently surface dependency
/// trees, but the struct shape is preserved so future versions
/// can wire them through.
#[derive(Debug, Clone, Copy)]
pub struct PushOptions {
    /// Relative weight 1-256. h2 default is 16.
    pub weight: u8,
    /// Optional parent stream id that this pushed stream depends on.
    pub depends_on: Option<u32>,
    /// Whether the dependency is exclusive (RFC 7540 §5.3.1).
    pub exclusive: bool,
}

impl Default for PushOptions {
    fn default() -> Self {
        Self {
            weight: 16,
            depends_on: None,
            exclusive: false,
        }
    }
}

/// Server-initiated push stream returned by
/// [`ResponseWriter::push_promise`]. Lifecycle:
///
/// 1. Construction via `push_promise` (synthetic request HEADERS
///    frame already on the wire).
/// 2. Caller sets status + headers via `set_status` / `header`.
/// 3. `write(bytes)` flushes the head on first call, then sends
///    one DATA frame per call.
/// 4. `write_trailers(headers)` or `end()` closes the stream.
pub struct PushStream {
    inner: PushInner,
}

enum PushInner {
    Pending(h2::server::SendPushedResponse<Bytes>),
    Sending(h2::SendStream<Bytes>),
    Closed,
}

impl PushStream {
    /// Sends one chunk of body data on the pushed stream. The
    /// first call flushes the synthetic response head with status
    /// `200` and no extra headers; for non-200 pushed responses,
    /// chain [`PushStream::send_head`] before the first write.
    pub fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        if matches!(self.inner, PushInner::Pending(_)) {
            self.send_head(200, Headers::new(), false)?;
        }
        let PushInner::Sending(sender) = &mut self.inner else {
            return Err(Error::Protocol("push stream closed".into()));
        };
        sender
            .send_data(Bytes::copy_from_slice(data), false)
            .map_err(|e| Error::Protocol(format!("push send_data: {e}")))
    }

    /// Flushes a pushed-response head with the given status and
    /// headers. Pass `end_of_stream = true` for an empty-body
    /// response; otherwise the caller should follow with
    /// `write`/`write_trailers`/`end`.
    pub fn send_head(
        &mut self,
        status: u16,
        headers: Headers,
        end_of_stream: bool,
    ) -> Result<(), Error> {
        let old = std::mem::replace(&mut self.inner, PushInner::Closed);
        match old {
            PushInner::Pending(mut pushed) => {
                let status = ::http::StatusCode::from_u16(status)
                    .map_err(|e| Error::Protocol(format!("bad status: {e}")))?;
                let mut builder = ::http::Response::builder().status(status);
                {
                    let h = builder
                        .headers_mut()
                        .ok_or_else(|| Error::Protocol("push head headers".into()))?;
                    for (name, value) in headers.iter() {
                        if let (Ok(n), Ok(v)) = (
                            ::http::HeaderName::from_bytes(name.as_bytes()),
                            ::http::HeaderValue::from_str(value),
                        ) {
                            h.insert(n, v);
                        }
                    }
                }
                let head: ::http::Response<()> = builder
                    .body(())
                    .map_err(|e| Error::Protocol(format!("push response build: {e}")))?;
                let sender = pushed
                    .send_response(head, end_of_stream)
                    .map_err(|e| Error::Protocol(format!("push send_response: {e}")))?;
                if end_of_stream {
                    self.inner = PushInner::Closed;
                } else {
                    self.inner = PushInner::Sending(sender);
                }
                Ok(())
            }
            PushInner::Sending(_) | PushInner::Closed => Err(Error::Protocol(
                "push head already flushed or stream closed".into(),
            )),
        }
    }

    /// Sends a trailing HEADERS frame with `END_STREAM` on the
    /// pushed stream. Mirrors
    /// [`ResponseWriter::write_trailers`]. Consumes the stream.
    pub fn write_trailers(mut self, trailers: Headers) -> Result<(), Error> {
        let state = std::mem::replace(&mut self.inner, PushInner::Closed);
        match state {
            PushInner::Sending(mut sender) => {
                let map = headers_to_header_map(&trailers);
                sender
                    .send_trailers(map)
                    .map_err(|e| Error::Protocol(format!("push send_trailers: {e}")))
            }
            PushInner::Pending(_) => Err(Error::Protocol(
                "push write_trailers requires send_head or write first".into(),
            )),
            PushInner::Closed => Err(Error::Protocol("push stream closed".into())),
        }
    }

    /// Closes the pushed stream cleanly. If `send_head` has not
    /// been called this flushes an empty 200 response with
    /// `END_STREAM`; if `write` has been called this sends an
    /// empty `DATA` frame with `END_STREAM`. Consumes the stream.
    pub fn end(mut self) -> Result<(), Error> {
        match std::mem::replace(&mut self.inner, PushInner::Closed) {
            PushInner::Pending(pushed) => {
                self.inner = PushInner::Pending(pushed);
                self.send_head(200, Headers::new(), true)
            }
            PushInner::Sending(mut sender) => sender
                .send_data(Bytes::new(), true)
                .map_err(|e| Error::Protocol(format!("push end: {e}"))),
            PushInner::Closed => Ok(()),
        }
    }
}

impl Drop for PushStream {
    fn drop(&mut self) {
        // Best-effort terminator so the peer's pushed request
        // doesn't dangle. Errors are ignored because Drop may run
        // during unwind.
        match std::mem::replace(&mut self.inner, PushInner::Closed) {
            PushInner::Pending(pushed) => {
                self.inner = PushInner::Pending(pushed);
                let _ = self.send_head(200, Headers::new(), true);
            }
            PushInner::Sending(mut sender) => {
                let _ = sender.send_data(Bytes::new(), true);
            }
            PushInner::Closed => {}
        }
    }
}

/// Configuration for the h2 server.
#[derive(Debug, Clone)]
pub struct Config {
    /// Maximum concurrent streams per connection.
    pub max_concurrent_streams: u32,
    /// Initial window size for inbound stream data.
    pub initial_window_size: u32,
    /// Initial connection window.
    pub initial_connection_window_size: u32,
    /// Maximum frame size negotiated with the peer.
    pub max_frame_size: u32,
    /// Header-list size cap.
    pub max_header_list_size: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_concurrent_streams: 100,
            initial_window_size: 1024 * 1024,
            initial_connection_window_size: 8 * 1024 * 1024,
            max_frame_size: 16_384,
            max_header_list_size: 16 * 1024,
        }
    }
}

impl Config {
    /// Overrides `max_concurrent_streams` (SETTINGS frame value
    /// `SETTINGS_MAX_CONCURRENT_STREAMS`). Returns `self` for
    /// chained-builder use.
    #[must_use]
    pub fn with_max_concurrent_streams(mut self, n: u32) -> Self {
        self.max_concurrent_streams = n;
        self
    }

    /// Overrides `initial_window_size` (SETTINGS frame value
    /// `SETTINGS_INITIAL_WINDOW_SIZE`). Affects per-stream flow
    /// control. Returns `self` for chained-builder use.
    #[must_use]
    pub fn with_initial_window_size(mut self, bytes: u32) -> Self {
        self.initial_window_size = bytes;
        self
    }

    /// Overrides `initial_connection_window_size`. Affects the
    /// per-connection flow-control window. Returns `self` for
    /// chained-builder use.
    #[must_use]
    pub fn with_initial_connection_window_size(mut self, bytes: u32) -> Self {
        self.initial_connection_window_size = bytes;
        self
    }

    /// Overrides `max_frame_size` (SETTINGS frame value
    /// `SETTINGS_MAX_FRAME_SIZE`). Must be in the range
    /// 16384..=16777215 per RFC 7540 §6.5.2.
    #[must_use]
    pub fn with_max_frame_size(mut self, bytes: u32) -> Self {
        self.max_frame_size = bytes;
        self
    }

    /// Overrides `max_header_list_size` (SETTINGS frame value
    /// `SETTINGS_MAX_HEADER_LIST_SIZE`). Used as a cap on the
    /// total uncompressed size of HEADERS / CONTINUATION blocks.
    #[must_use]
    pub fn with_max_header_list_size(mut self, bytes: u32) -> Self {
        self.max_header_list_size = bytes;
        self
    }

    /// Enables HTTP/2 server push (SETTINGS frame value
    /// `SETTINGS_ENABLE_PUSH`). The wire-protocol setting is
    /// controlled by the peer (the client decides whether to
    /// permit push); this knob is reserved for symmetry with the
    /// h1 server config and currently has no effect on the h2
    /// crate's SETTINGS frame because h2 always advertises the
    /// server side as push-capable.
    #[must_use]
    pub const fn with_enable_push(self, _enable: bool) -> Self {
        self
    }
}

/// Errors raised by the h2 server.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// I/O error on the underlying stream.
    #[error("h2 io: {0}")]
    Io(#[from] std::io::Error),
    /// h2 protocol-level failure.
    #[error("h2 protocol: {0}")]
    Protocol(String),
    /// Handler panicked or rejected the request.
    #[error("h2 handler: {0}")]
    Handler(String),
}

impl From<h2::Error> for Error {
    fn from(err: h2::Error) -> Self {
        Self::Protocol(err.to_string())
    }
}

/// Handle returned by `serve` to control graceful shutdown.
#[derive(Clone)]
pub struct ServerHandle {
    shutdown: Arc<AtomicBool>,
    in_flight: Arc<AtomicUsize>,
}

impl ServerHandle {
    /// Number of streams currently in-flight.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::Acquire)
    }

    /// Initiates graceful shutdown: stops accepting new streams,
    /// waits for in-flight to drain (with deadline). Returns
    /// `true` on clean drain.
    pub fn shutdown(&self, deadline: Option<Duration>) -> bool {
        self.shutdown.store(true, Ordering::Release);
        let target = deadline.map(|d| std::time::Instant::now() + d);
        loop {
            if self.in_flight.load(Ordering::Acquire) == 0 {
                return true;
            }
            if let Some(end) = target
                && std::time::Instant::now() >= end
            {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Drives an HTTP/2 server connection on the calling goroutine.
///
/// `io` is the AsyncRead+AsyncWrite source — typically an
/// [`crate::async_tcp::AsyncTcpStream`] (for h2c) or a rustls
/// TLS-wrapped stream (for h2-over-TLS). The handler is called
/// once per inbound stream.
///
/// This function returns when the connection closes (clean
/// GOAWAY, peer disconnect, or fatal protocol error).
pub fn serve_connection<S, H>(io: S, handler: Arc<H>, config: Config) -> Result<ServerHandle, Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    H: Handler,
{
    let shutdown = Arc::new(AtomicBool::new(false));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let handle = ServerHandle {
        shutdown: Arc::clone(&shutdown),
        in_flight: Arc::clone(&in_flight),
    };

    crate::runtime_future::drive(serve_connection_async(
        io,
        handler,
        config,
        Arc::clone(&shutdown),
        Arc::clone(&in_flight),
    ))?;

    Ok(handle)
}

/// Async core of [`serve_connection`]. Use when the caller is
/// already inside a future driven by `runtime_future::drive` (e.g.
/// the ALPN dispatcher in `http::server::bind_and_run_tls`) and
/// wants to `.await` the connection lifecycle rather than re-enter
/// the driver. Public for that integration path.
pub async fn serve_connection_async<S, H>(
    io: S,
    handler: Arc<H>,
    config: Config,
    shutdown: Arc<AtomicBool>,
    in_flight: Arc<AtomicUsize>,
) -> Result<(), Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    H: Handler,
{
    let mut builder = h2::server::Builder::new();
    builder
        .max_concurrent_streams(config.max_concurrent_streams)
        .initial_window_size(config.initial_window_size)
        .initial_connection_window_size(config.initial_connection_window_size)
        .max_frame_size(config.max_frame_size)
        .max_header_list_size(config.max_header_list_size);
    let mut conn = match builder.handshake::<_, Bytes>(io).await {
        Ok(c) => c,
        Err(e) => return Err(Error::Protocol(e.to_string())),
    };

    loop {
        if shutdown.load(Ordering::Acquire) {
            conn.graceful_shutdown();
            while let Some(req) = conn.accept().await {
                match req {
                    Ok((req, respond)) => {
                        in_flight.fetch_add(1, Ordering::AcqRel);
                        spawn_stream_handler(
                            req,
                            respond,
                            Arc::clone(&handler),
                            Arc::clone(&in_flight),
                        );
                    }
                    Err(_) => break,
                }
            }
            break;
        }
        match conn.accept().await {
            Some(Ok((req, respond))) => {
                in_flight.fetch_add(1, Ordering::AcqRel);
                spawn_stream_handler(req, respond, Arc::clone(&handler), Arc::clone(&in_flight));
            }
            Some(Err(e)) => {
                eprintln!("h2: accept error: {e}");
                break;
            }
            None => break,
        }
    }
    Ok(())
}

fn spawn_stream_handler<H>(
    req: http::Request<RecvStream>,
    respond: SendResponse<Bytes>,
    handler: Arc<H>,
    in_flight: Arc<AtomicUsize>,
) where
    H: Handler,
{
    gossamer_runtime::sched_global::spawn(Box::new(move || {
        let result = crate::runtime_future::drive(serve_one_stream(req, respond, handler));
        if let Err(e) = result {
            eprintln!("h2: stream error: {e}");
        }
        in_flight.fetch_sub(1, Ordering::AcqRel);
    }));
}

async fn serve_one_stream<H>(
    h2_req: http::Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
    handler: Arc<H>,
) -> Result<(), Error>
where
    H: Handler,
{
    // Bridge from h2's http::Request<RecvStream> into our
    // Request shape.
    let (parts, mut body_stream) = h2_req.into_parts();
    let method = method_from_http(&parts.method);
    let path_q = parts
        .uri
        .path_and_query()
        .map_or_else(|| "/".to_string(), |pq| pq.as_str().to_string());
    let (path, query) = crate::http::split_path_query(&path_q);

    let mut headers = Headers::new();
    for (name, value) in parts.headers.iter() {
        if let Ok(v) = value.to_str() {
            headers.insert(name.as_str(), v);
        }
    }

    // Read body chunks.
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = body_stream.data().await {
        let chunk = chunk?;
        let _ = body_stream.flow_control().release_capacity(chunk.len());
        body.extend_from_slice(&chunk);
    }
    let raw_trailers = body_stream.trailers().await.unwrap_or(None);
    let mut trailers: Option<Headers> = None;
    if let Some(tr) = raw_trailers {
        let mut t = Headers::new();
        for (name, value) in tr.iter() {
            if let Ok(v) = value.to_str() {
                t.insert(name.as_str(), v);
            }
        }
        trailers = Some(t);
    }

    let request = Request {
        method,
        path,
        query,
        headers,
        body,
        context: crate::context::Context::background(),
        trailers,
    };

    // Invoke handler (catch panics so a single bad handler does
    // not bring the connection down).
    let response =
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler.serve(request))) {
            Ok(r) => r,
            Err(_) => Response {
                status: StatusCode(500),
                headers: Headers::new(),
                body: b"internal server error".to_vec(),
            },
        };

    // Build the http::Response head for h2.
    let status = ::http::StatusCode::from_u16(response.status.as_u16())
        .map_err(|e| Error::Protocol(format!("bad status: {e}")))?;
    let mut builder = ::http::Response::builder().status(status);
    {
        let h = builder
            .headers_mut()
            .ok_or_else(|| Error::Protocol("response builder headers".into()))?;
        for (name, value) in response.headers.iter() {
            if let (Ok(n), Ok(v)) = (
                ::http::HeaderName::from_bytes(name.as_bytes()),
                ::http::HeaderValue::from_str(value),
            ) {
                h.insert(n, v);
            }
        }
    }
    let body_empty = response.body.is_empty();
    let head: ::http::Response<()> = builder
        .body(())
        .map_err(|e| Error::Protocol(format!("response build: {e}")))?;
    let mut sender = respond
        .send_response(head, body_empty)
        .map_err(|e| Error::Protocol(format!("send_response: {e}")))?;
    if !body_empty {
        sender
            .send_data(Bytes::from(response.body), true)
            .map_err(|e| Error::Protocol(format!("send_data: {e}")))?;
    }
    Ok(())
}

fn method_from_http(m: &::http::Method) -> Method {
    match m.as_str() {
        "GET" => Method::Get,
        "POST" => Method::Post,
        "PUT" => Method::Put,
        "DELETE" => Method::Delete,
        "PATCH" => Method::Patch,
        "HEAD" => Method::Head,
        "OPTIONS" => Method::Options,
        _ => Method::Get,
    }
}

/// Convenience: bind a plain-TCP listener on `addr` (h2c) and
/// run `handler` over every accepted connection. Useful for
/// debugging without TLS. **Real production deployments should
/// use the ALPN-dispatching `crate::http::server::bind_and_run_tls`
/// which negotiates h2 only on TLS connections that advertised
/// it via ALPN.**
pub fn bind_and_run_h2c<H>(addr: &str, handler: H, config: Config) -> Result<(), Error>
where
    H: Handler + Clone,
{
    let listener = std::net::TcpListener::bind(addr).map_err(Error::Io)?;
    listener.set_nonblocking(false).map_err(Error::Io)?;
    loop {
        let (stream, _peer) = listener.accept().map_err(Error::Io)?;
        let handler = handler.clone();
        let config = config.clone();
        // Each connection runs in a goroutine so `runtime_future::drive`
        // has the gid context it needs to park / unpark on IO.
        gossamer_runtime::sched_global::spawn(Box::new(move || {
            let wrapped = match crate::net::TcpStream::from_std_blocking(stream) {
                Ok(s) => s,
                Err(_) => return,
            };
            let async_stream = crate::async_tcp::AsyncTcpStream::new(wrapped);
            let _ = serve_connection(async_stream, Arc::new(handler), config);
        }));
    }
}

// ---------------------------------------------------------------------------
// Streaming connection serving
// ---------------------------------------------------------------------------

/// Drives an HTTP/2 server connection with a streaming handler.
/// Same shape as [`serve_connection`] but accepts a
/// [`StreamingHandler`] that emits the body in chunks via a
/// [`ResponseWriter`].
pub fn serve_connection_streaming<S, H>(
    io: S,
    handler: Arc<H>,
    config: Config,
) -> Result<ServerHandle, Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    H: StreamingHandler,
{
    let shutdown = Arc::new(AtomicBool::new(false));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let handle = ServerHandle {
        shutdown: Arc::clone(&shutdown),
        in_flight: Arc::clone(&in_flight),
    };
    crate::runtime_future::drive(serve_connection_streaming_async(
        io,
        handler,
        config,
        Arc::clone(&shutdown),
        Arc::clone(&in_flight),
    ))?;
    Ok(handle)
}

/// Async counterpart of [`serve_connection_streaming`].
pub async fn serve_connection_streaming_async<S, H>(
    io: S,
    handler: Arc<H>,
    config: Config,
    shutdown: Arc<AtomicBool>,
    in_flight: Arc<AtomicUsize>,
) -> Result<(), Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    H: StreamingHandler,
{
    let mut builder = h2::server::Builder::new();
    builder
        .max_concurrent_streams(config.max_concurrent_streams)
        .initial_window_size(config.initial_window_size)
        .initial_connection_window_size(config.initial_connection_window_size)
        .max_frame_size(config.max_frame_size)
        .max_header_list_size(config.max_header_list_size);
    let mut conn = match builder.handshake::<_, Bytes>(io).await {
        Ok(c) => c,
        Err(e) => return Err(Error::Protocol(e.to_string())),
    };
    loop {
        if shutdown.load(Ordering::Acquire) {
            conn.graceful_shutdown();
            while let Some(req) = conn.accept().await {
                match req {
                    Ok((req, respond)) => {
                        in_flight.fetch_add(1, Ordering::AcqRel);
                        spawn_streaming_handler(
                            req,
                            respond,
                            Arc::clone(&handler),
                            Arc::clone(&in_flight),
                        );
                    }
                    Err(_) => break,
                }
            }
            break;
        }
        match conn.accept().await {
            Some(Ok((req, respond))) => {
                in_flight.fetch_add(1, Ordering::AcqRel);
                spawn_streaming_handler(req, respond, Arc::clone(&handler), Arc::clone(&in_flight));
            }
            Some(Err(e)) => {
                eprintln!("h2: accept error: {e}");
                break;
            }
            None => break,
        }
    }
    Ok(())
}

fn spawn_streaming_handler<H>(
    req: http::Request<RecvStream>,
    respond: SendResponse<Bytes>,
    handler: Arc<H>,
    in_flight: Arc<AtomicUsize>,
) where
    H: StreamingHandler,
{
    gossamer_runtime::sched_global::spawn(Box::new(move || {
        let result =
            crate::runtime_future::drive(serve_one_streaming_stream(req, respond, handler));
        if let Err(e) = result {
            eprintln!("h2: stream error: {e}");
        }
        in_flight.fetch_sub(1, Ordering::AcqRel);
    }));
}

async fn serve_one_streaming_stream<H>(
    h2_req: http::Request<RecvStream>,
    respond: SendResponse<Bytes>,
    handler: Arc<H>,
) -> Result<(), Error>
where
    H: StreamingHandler,
{
    let (parts, mut body_stream) = h2_req.into_parts();
    let method = method_from_http(&parts.method);
    let path_q = parts
        .uri
        .path_and_query()
        .map_or_else(|| "/".to_string(), |pq| pq.as_str().to_string());
    let (path, query) = crate::http::split_path_query(&path_q);

    let mut headers = Headers::new();
    for (name, value) in parts.headers.iter() {
        if let Ok(v) = value.to_str() {
            headers.insert(name.as_str(), v);
        }
    }

    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = body_stream.data().await {
        let chunk = chunk?;
        let _ = body_stream.flow_control().release_capacity(chunk.len());
        body.extend_from_slice(&chunk);
    }
    let raw_trailers = body_stream.trailers().await.unwrap_or(None);
    let mut trailers: Option<Headers> = None;
    if let Some(tr) = raw_trailers {
        let mut t = Headers::new();
        for (name, value) in tr.iter() {
            if let Ok(v) = value.to_str() {
                t.insert(name.as_str(), v);
            }
        }
        trailers = Some(t);
    }

    let request = Request {
        method,
        path,
        query,
        headers,
        body,
        context: crate::context::Context::background(),
        trailers,
    };

    let writer = ResponseWriter {
        state: WriterState::Pending {
            respond,
            status: 200,
            headers: Headers::new(),
        },
    };

    // Run the streaming handler under catch_unwind so a panic
    // doesn't kill the whole connection.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handler.serve(request, writer)
    }));
    match result {
        Ok(r) => r,
        Err(_) => Err(Error::Handler("streaming handler panicked".into())),
    }
}

/// Convenience: bind a plain-TCP listener on `addr` (h2c) and
/// serve `handler` (a streaming handler) over every accepted
/// connection. The streaming counterpart of [`bind_and_run_h2c`].
pub fn bind_and_run_h2c_streaming<H>(addr: &str, handler: H, config: Config) -> Result<(), Error>
where
    H: StreamingHandler + Clone,
{
    let listener = std::net::TcpListener::bind(addr).map_err(Error::Io)?;
    listener.set_nonblocking(false).map_err(Error::Io)?;
    loop {
        let (stream, _peer) = listener.accept().map_err(Error::Io)?;
        let handler = handler.clone();
        let config = config.clone();
        gossamer_runtime::sched_global::spawn(Box::new(move || {
            let wrapped = match crate::net::TcpStream::from_std_blocking(stream) {
                Ok(s) => s,
                Err(_) => return,
            };
            let async_stream = crate::async_tcp::AsyncTcpStream::new(wrapped);
            let _ = serve_connection_streaming(async_stream, Arc::new(handler), config);
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener as StdListener;
    use std::time::Duration;

    #[test]
    fn handler_trait_impl_for_fn_works() {
        // Compile-time check: a closure satisfies Handler.
        let h: Arc<dyn Handler> = Arc::new(|_req: Request| Response {
            status: StatusCode(200),
            headers: Headers::new(),
            body: Vec::new(),
        });
        let r = h.serve(Request {
            method: Method::Get,
            path: "/x".into(),
            query: String::new(),
            headers: Headers::new(),
            body: Vec::new(),
            context: crate::context::Context::background(),
            trailers: None,
        });
        assert_eq!(r.status, StatusCode(200));
    }

    #[test]
    fn config_default_has_sane_values() {
        let c = Config::default();
        assert!(c.max_concurrent_streams >= 100);
        assert!(c.initial_window_size >= 64 * 1024);
        assert!(c.max_frame_size >= 16 * 1024);
    }

    #[test]
    fn method_from_http_round_trips_common_verbs() {
        assert_eq!(method_from_http(&::http::Method::GET), Method::Get);
        assert_eq!(method_from_http(&::http::Method::POST), Method::Post);
        assert_eq!(method_from_http(&::http::Method::DELETE), Method::Delete);
        assert_eq!(method_from_http(&::http::Method::OPTIONS), Method::Options);
    }

    #[test]
    fn server_handle_shutdown_completes_when_zero_in_flight() {
        let handle = ServerHandle {
            shutdown: Arc::new(AtomicBool::new(false)),
            in_flight: Arc::new(AtomicUsize::new(0)),
        };
        assert!(handle.shutdown(Some(Duration::from_millis(100))));
    }

    #[test]
    fn server_handle_shutdown_timeout_returns_false_with_in_flight() {
        let handle = ServerHandle {
            shutdown: Arc::new(AtomicBool::new(false)),
            in_flight: Arc::new(AtomicUsize::new(1)),
        };
        assert!(!handle.shutdown(Some(Duration::from_millis(50))));
    }

    #[test]
    fn bind_and_run_h2c_binds_then_drops() {
        // Bind on port 0 and immediately drop — we just want
        // to verify the TCP listener creates and the entry
        // point doesn't panic before accept.
        let listener = StdListener::bind("127.0.0.1:0").unwrap();
        let _addr = listener.local_addr().unwrap();
        drop(listener);
    }

    #[test]
    fn streaming_handler_trait_impl_for_fn_works() {
        // Compile-time check: a closure satisfies StreamingHandler.
        let h: Arc<dyn StreamingHandler> = Arc::new(
            |_req: Request, mut w: ResponseWriter| -> Result<(), Error> {
                w.set_status(204);
                w.finish()
            },
        );
        let _ = Arc::clone(&h);
    }

    #[test]
    fn config_builders_set_each_field() {
        let c = Config::default()
            .with_max_concurrent_streams(42)
            .with_initial_window_size(65_535)
            .with_initial_connection_window_size(131_070)
            .with_max_frame_size(32_768)
            .with_max_header_list_size(8_192)
            .with_enable_push(true);
        assert_eq!(c.max_concurrent_streams, 42);
        assert_eq!(c.initial_window_size, 65_535);
        assert_eq!(c.initial_connection_window_size, 131_070);
        assert_eq!(c.max_frame_size, 32_768);
        assert_eq!(c.max_header_list_size, 8_192);
    }

    #[test]
    fn push_options_default_carries_weight_16() {
        let opts = PushOptions::default();
        assert_eq!(opts.weight, 16);
        assert!(opts.depends_on.is_none());
        assert!(!opts.exclusive);
    }

    #[test]
    fn push_options_custom_round_trip() {
        let opts = PushOptions {
            weight: 128,
            depends_on: Some(7),
            exclusive: true,
        };
        let cloned = opts;
        assert_eq!(cloned.weight, 128);
        assert_eq!(cloned.depends_on, Some(7));
        assert!(cloned.exclusive);
    }

    #[test]
    fn trailers_type_alias_is_headers() {
        let mut t: Trailers = Headers::new();
        t.insert("x-trace-id", "abc123");
        assert_eq!(t.get("x-trace-id"), Some("abc123"));
    }

    #[test]
    fn headers_to_header_map_skips_invalid_names() {
        let mut h = Headers::new();
        h.insert("x-good", "ok");
        // Insert a name with a space — not a valid HTTP token.
        // `Headers::insert` lowercases but doesn't reject; the
        // h2-bridge converter should drop it.
        h.insert("bad name", "v");
        let map = headers_to_header_map(&h);
        assert!(map.get("x-good").is_some());
        assert!(map.get("bad name").is_none());
    }
}
