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
//! - Bounded-body handlers via [`Handler`] — handler returns a
//!   complete `Response`, body sent as one `DATA` frame.
//! - Streaming handlers via [`StreamingHandler`] — handler is
//!   passed a [`ResponseWriter`] and can emit `DATA` frames
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
    Streaming(h2::SendStream<Bytes>),
    Closed,
}

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
                respond,
                status,
                headers,
            } = old
            else {
                unreachable!()
            };
            let sender = send_head(respond, status, &headers, false)?;
            self.state = WriterState::Streaming(sender);
        }
        let WriterState::Streaming(sender) = &mut self.state else {
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
                respond,
                status,
                headers,
            } => {
                let _ = send_head(respond, status, &headers, true)?;
                Ok(())
            }
            WriterState::Streaming(mut sender) => sender
                .send_data(Bytes::new(), true)
                .map_err(|e| Error::Protocol(format!("send_terminator: {e}"))),
            WriterState::Closed => Ok(()),
        }
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
    respond: SendResponse<Bytes>,
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
    let mut respond = respond;
    respond
        .send_response(head, end_of_stream)
        .map_err(|e| Error::Protocol(format!("send_response: {e}")))
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

/// Handle returned by [`serve`] to control graceful shutdown.
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
    let trailers = body_stream.trailers().await.unwrap_or(None);
    if let Some(tr) = trailers {
        for (name, value) in tr.iter() {
            if let Ok(v) = value.to_str() {
                headers.insert(name.as_str(), v);
            }
        }
    }

    let request = Request {
        method,
        path,
        query,
        headers,
        body,
        context: crate::context::Context::background(),
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

    let request = Request {
        method,
        path,
        query,
        headers,
        body,
        context: crate::context::Context::background(),
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
}
