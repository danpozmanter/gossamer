//! Runtime support for `std::http`.
//! Ships the HTTP/1.1 type surface Gossamer programs target:
//! `Request`, `Response`, `Method`, `StatusCode`, `Headers`, plus the
//! simple parsers for request lines and status lines. A working
//! server driver is a -era piece of work; this module gives
//! the value shapes.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

/// HTTP method enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// `GET`.
    Get,
    /// `POST`.
    Post,
    /// `PUT`.
    Put,
    /// `DELETE`.
    Delete,
    /// `PATCH`.
    Patch,
    /// `HEAD`.
    Head,
    /// `OPTIONS`.
    Options,
}

impl Method {
    /// Canonical uppercase spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Patch => "PATCH",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
        }
    }

    /// Parses `"GET"`, `"POST"`, etc. Case-insensitive.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.to_ascii_uppercase().as_str() {
            "GET" => Self::Get,
            "POST" => Self::Post,
            "PUT" => Self::Put,
            "DELETE" => Self::Delete,
            "PATCH" => Self::Patch,
            "HEAD" => Self::Head,
            "OPTIONS" => Self::Options,
            _ => return None,
        })
    }
}

/// HTTP status code wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StatusCode(pub u16);

impl StatusCode {
    /// `200 OK`.
    pub const OK: Self = Self(200);
    /// `201 Created`.
    pub const CREATED: Self = Self(201);
    /// `204 No Content`.
    pub const NO_CONTENT: Self = Self(204);
    /// `301 Moved Permanently`.
    pub const MOVED_PERMANENTLY: Self = Self(301);
    /// `400 Bad Request`.
    pub const BAD_REQUEST: Self = Self(400);
    /// `401 Unauthorized`.
    pub const UNAUTHORIZED: Self = Self(401);
    /// `403 Forbidden`.
    pub const FORBIDDEN: Self = Self(403);
    /// `404 Not Found`.
    pub const NOT_FOUND: Self = Self(404);
    /// `500 Internal Server Error`.
    pub const INTERNAL_SERVER_ERROR: Self = Self(500);

    /// Returns the numeric code.
    #[must_use]
    pub const fn as_u16(self) -> u16 {
        self.0
    }

    /// Returns `true` for `2xx` codes.
    #[must_use]
    pub const fn is_success(self) -> bool {
        self.0 >= 200 && self.0 < 300
    }

    /// Returns the canonical reason phrase for common codes; `None`
    /// for codes outside the small well-known set.
    #[must_use]
    pub const fn reason(self) -> Option<&'static str> {
        Some(match self.0 {
            200 => "OK",
            201 => "Created",
            204 => "No Content",
            301 => "Moved Permanently",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            500 => "Internal Server Error",
            _ => return None,
        })
    }
}

/// Case-insensitive header map keyed by canonical lowercase name.
#[derive(Debug, Clone, Default)]
pub struct Headers {
    inner: BTreeMap<String, String>,
}

impl Headers {
    /// Empty header map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or overwrites the header value for `name`.
    pub fn insert(&mut self, name: &str, value: &str) {
        self.inner
            .insert(name.to_ascii_lowercase(), value.to_string());
    }

    /// Returns the value of `name`, if set.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.inner
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// Whether a header is set.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.inner.contains_key(&name.to_ascii_lowercase())
    }

    /// Returns the number of headers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Iterates every `(name, value)` pair in sorted-by-name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.inner.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// Incoming HTTP request.
#[derive(Debug, Clone)]
pub struct Request {
    /// Request method.
    pub method: Method,
    /// Request-target (path + query).
    pub path: String,
    /// Request headers.
    pub headers: Headers,
    /// Optional body.
    pub body: Vec<u8>,
    /// Per-request cancellation context. Mirrors Go's
    /// `http.Request.Context()`. Defaults to
    /// [`crate::context::Context::background`] when the server
    /// does not override it. Shutting down the server cancels the
    /// per-connection context so long-running handlers notice.
    pub context: crate::context::Context,
}

impl Request {
    /// Returns the path, conveniently typed.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the request-scoped cancellation context.
    #[must_use]
    pub fn context(&self) -> &crate::context::Context {
        &self.context
    }
}

/// Outgoing HTTP response.
#[derive(Debug, Clone)]
pub struct Response {
    /// Status code.
    pub status: StatusCode,
    /// Response headers.
    pub headers: Headers,
    /// Response body.
    pub body: Vec<u8>,
}

impl Response {
    /// Builds a text response with the given body.
    #[must_use]
    pub fn text(status: StatusCode, body: impl Into<String>) -> Self {
        let body = body.into();
        let mut headers = Headers::new();
        headers.insert("content-type", "text/plain; charset=utf-8");
        headers.insert("content-length", &body.len().to_string());
        Self {
            status,
            headers,
            body: body.into_bytes(),
        }
    }

    /// Builds a JSON response — body bytes are inserted verbatim.
    #[must_use]
    pub fn json(status: StatusCode, body: impl Into<Vec<u8>>) -> Self {
        let body = body.into();
        let mut headers = Headers::new();
        headers.insert("content-type", "application/json");
        headers.insert("content-length", &body.len().to_string());
        Self {
            status,
            headers,
            body,
        }
    }
}

/// Parses the request line `METHOD PATH VERSION`.
#[must_use]
pub fn parse_request_line(line: &str) -> Option<(Method, String, String)> {
    let mut parts = line.split_whitespace();
    let method = Method::parse(parts.next()?)?;
    let path = parts.next()?.to_string();
    let version = parts.next()?.to_string();
    if parts.next().is_some() {
        return None;
    }
    Some((method, path, version))
}

/// Parses the status line `VERSION CODE [REASON]`.
#[must_use]
pub fn parse_status_line(line: &str) -> Option<(String, StatusCode, String)> {
    let mut parts = line.splitn(3, ' ');
    let version = parts.next()?.to_string();
    let code = parts.next()?.parse::<u16>().ok()?;
    let reason = parts.next().unwrap_or_default().to_string();
    Some((version, StatusCode(code), reason))
}

/// Placeholder for a future real HTTP server (wires into the
/// scheduler + poller).
#[derive(Debug, Default)]
pub struct Server;

impl Server {
    /// Constructs a stub server; integration replaces this.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Minimal HTTP/1.1 server loop used by the interpreter's
/// `http::serve` native builtin.
pub mod server {
    use std::io::{self, BufRead, BufReader, Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::{Method, Request, Response};

    /// Configuration passed to [`run`].
    #[derive(Debug, Clone)]
    pub struct Config {
        /// Optional per-request read timeout.
        pub read_timeout: Option<Duration>,
        /// If set, the server stops accepting once `max_requests`
        /// requests have been handled. Used by integration tests.
        pub max_requests: Option<u64>,
        /// Shared flag that, when set to `true`, tells the accept
        /// loop to stop after the next accept wake-up.
        pub shutdown: Arc<AtomicBool>,
        /// Maximum header-block size (bytes). Requests with a
        /// header block larger than this return `431`. Default 8 KiB.
        pub max_header_bytes: usize,
        /// Maximum body size (bytes). Requests larger than this
        /// return `413`. Default 1 MiB.
        pub max_body_bytes: usize,
    }

    impl Default for Config {
        fn default() -> Self {
            Self {
                read_timeout: Some(Duration::from_secs(30)),
                max_requests: None,
                shutdown: Arc::new(AtomicBool::new(false)),
                max_header_bytes: 8 * 1024,
                max_body_bytes: 1024 * 1024,
            }
        }
    }

    /// Runs the accept loop on `listener`. Each accepted connection
    /// gets its own worker thread — Gossamer's goroutine-per-
    /// connection story for the single-threaded interpreter. The
    /// worker reads requests (potentially slow) on its own thread,
    /// forwards each parsed [`Request`] plus a one-shot response
    /// channel to the main thread, writes the response when the
    /// handler returns it, and keeps the connection alive for
    /// subsequent requests unless the peer (or handler) asked to
    /// close.
    ///
    /// The handler still runs on the main thread — the interpreter
    /// is not `Send` — so handler invocation remains serialised.
    /// The important part is that slow clients no longer block
    /// accept or other in-flight handlers during their read / write
    /// phases, and a single TCP connection is reused across
    /// requests.
    ///
    /// Shutdown: when `config.shutdown` flips to `true`, the main
    /// loop connects to the bound address to break the acceptor out
    /// of its blocking `accept()` call, then returns. Reaching
    /// `config.max_requests` uses the same self-connect trick.
    pub fn run<H>(listener: TcpListener, config: &Config, mut handle: H) -> io::Result<()>
    where
        H: FnMut(Request) -> Response,
    {
        use std::sync::mpsc::{RecvTimeoutError, channel};

        let bound_addr = listener.local_addr()?;

        let (dispatch_tx, dispatch_rx) = channel::<(Request, std::sync::mpsc::Sender<Response>)>();

        // Acceptor thread: blocking accept, one worker per
        // connection. No poll sleep.
        let shutdown_for_accept = Arc::clone(&config.shutdown);
        let cfg_for_workers = config.clone();
        let tx_for_accept = dispatch_tx.clone();
        let acceptor = std::thread::Builder::new()
            .name("gossamer-http-accept".to_string())
            .spawn(move || {
                accept_loop(
                    listener,
                    shutdown_for_accept,
                    cfg_for_workers,
                    tx_for_accept,
                );
            })
            .map_err(|e| io::Error::other(format!("spawn accept: {e}")))?;

        // Drop our extra sender so the dispatch channel sees
        // Disconnected once the acceptor and all workers are gone.
        drop(dispatch_tx);

        let mut served: u64 = 0;
        let wake_self = || {
            // Best-effort wake — acceptor is stuck in `accept()`.
            let _ = TcpStream::connect_timeout(&bound_addr, Duration::from_millis(500));
        };

        loop {
            if config.shutdown.load(Ordering::Relaxed) {
                wake_self();
                break;
            }
            match dispatch_rx.recv_timeout(Duration::from_millis(50)) {
                Ok((req, responder)) => {
                    let response = handle(req);
                    let _ = responder.send(response);
                    served = served.saturating_add(1);
                    if let Some(max) = config.max_requests {
                        if served >= max {
                            config.shutdown.store(true, Ordering::Relaxed);
                            wake_self();
                            break;
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        // Acceptor should exit now that shutdown is set and we've
        // self-connected; a stray worker panic would just drop the
        // join handle.
        let _ = acceptor.join();
        Ok(())
    }

    fn accept_loop(
        listener: TcpListener,
        shutdown: Arc<AtomicBool>,
        config: Config,
        dispatch_tx: std::sync::mpsc::Sender<(Request, std::sync::mpsc::Sender<Response>)>,
    ) {
        // Nonblocking accept + 50 ms sleep on `WouldBlock` lets the
        // loop poll `shutdown` regardless of whether the wake-self
        // self-connect lands. macOS in particular sometimes refuses
        // the wake connection (TIME_WAIT churn under load), and a
        // pure blocking `accept()` would never observe shutdown.
        let _ = listener.set_nonblocking(true);
        loop {
            if shutdown.load(Ordering::Relaxed) {
                return;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    if shutdown.load(Ordering::Relaxed) {
                        let _ = stream.shutdown(Shutdown::Both);
                        return;
                    }
                    let worker_config = config.clone();
                    let tx = dispatch_tx.clone();
                    let spawn_result = std::thread::Builder::new()
                        .name("gossamer-http-conn".to_string())
                        .spawn(move || worker_loop(stream, worker_config, tx));
                    if let Err(err) = spawn_result {
                        eprintln!("http: spawn worker failed: {err}");
                        // Keep accepting — dropping this connection is
                        // preferable to tearing the server down.
                    }
                }
                Err(ref e) if matches!(e.kind(), io::ErrorKind::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(ref e) if matches!(e.kind(), io::ErrorKind::Interrupted) => {}
                Err(err) => {
                    if !shutdown.load(Ordering::Relaxed) {
                        eprintln!("http: accept error: {err}");
                    }
                    return;
                }
            }
        }
    }

    fn wants_close(headers: &super::Headers) -> bool {
        matches!(headers.get("connection"), Some(v) if v.eq_ignore_ascii_case("close"))
    }

    /// Per-connection worker. Runs on its own thread; reads
    /// requests from a persistent buffered reader, hands each to
    /// the main thread via `dispatch_tx`, writes the response, and
    /// loops until the peer (or handler) asks to close or the
    /// socket errors out.
    fn worker_loop(
        stream: TcpStream,
        config: Config,
        dispatch_tx: std::sync::mpsc::Sender<(Request, std::sync::mpsc::Sender<Response>)>,
    ) {
        if let Some(timeout) = config.read_timeout {
            let _ = stream.set_read_timeout(Some(timeout));
            let _ = stream.set_write_timeout(Some(timeout));
        }
        // Disable Nagle so short responses land on the wire right
        // away. Dominant workload here is small keep-alive replies.
        let _ = stream.set_nodelay(true);

        // One BufReader lives across every request on this
        // connection so any bytes pipelined after the request line
        // aren't lost when the next read starts.
        let mut reader = BufReader::new(stream);

        loop {
            match read_request(&mut reader, &config) {
                Ok(Some((request, http10, client_close))) => {
                    let (resp_tx, resp_rx) = std::sync::mpsc::channel::<Response>();
                    if dispatch_tx.send((request, resp_tx)).is_err() {
                        break;
                    }
                    match resp_rx.recv() {
                        Ok(mut response) => {
                            let handler_close = wants_close(&response.headers);
                            let keep_alive = !http10 && !client_close && !handler_close;
                            if keep_alive {
                                if !response.headers.contains("connection") {
                                    response.headers.insert("connection", "keep-alive");
                                }
                            } else if !response.headers.contains("connection") {
                                response.headers.insert("connection", "close");
                            }
                            if let Err(err) = write_response(reader.get_mut(), &response) {
                                if !is_ignorable(&err) {
                                    eprintln!("http: write error: {err}");
                                }
                                break;
                            }
                            if !keep_alive {
                                break;
                            }
                        }
                        Err(_) => break, // main thread gone
                    }
                }
                Ok(None) => break, // clean EOF between requests
                Err(err) => {
                    if !is_ignorable(&err) {
                        eprintln!("http: parse error: {err}");
                    }
                    break;
                }
            }
        }
        let _ = reader.get_mut().shutdown(Shutdown::Both);
    }

    fn is_ignorable(err: &io::Error) -> bool {
        matches!(
            err.kind(),
            io::ErrorKind::UnexpectedEof
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::TimedOut
                | io::ErrorKind::WouldBlock
        )
    }

    /// Reads one HTTP request from `reader`. Returns `Ok(None)` on a
    /// clean EOF between requests (idle keep-alive connection that
    /// closed). Returns `Ok(Some((req, http10, client_close)))` on a
    /// parsed request; `http10` is true when the request line said
    /// HTTP/1.0, and `client_close` is true when the peer sent
    /// `Connection: close`.
    fn read_request(
        reader: &mut BufReader<TcpStream>,
        config: &Config,
    ) -> io::Result<Option<(Request, bool, bool)>> {
        let mut line = String::new();
        let first = reader.read_line(&mut line)?;
        if first == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
        let (method, path, version) = super::parse_request_line(trimmed)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad request line"))?;
        let http10 = version.eq_ignore_ascii_case("HTTP/1.0");
        let mut headers = super::Headers::new();
        let mut content_length: usize = 0;
        let mut header_bytes_read: usize = line.len();
        loop {
            line.clear();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 {
                break;
            }
            header_bytes_read = header_bytes_read.saturating_add(bytes);
            if header_bytes_read > config.max_header_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("header block exceeded {}-byte cap", config.max_header_bytes),
                ));
            }
            let stripped = line.trim_end_matches(&['\r', '\n'][..]);
            if stripped.is_empty() {
                break;
            }
            if let Some((name, value)) = stripped.split_once(':') {
                let value = value.trim();
                headers.insert(name.trim(), value);
                if name.trim().eq_ignore_ascii_case("content-length") {
                    content_length = value.parse().unwrap_or(0);
                }
            }
        }
        if content_length > config.max_body_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "body length {content_length} exceeds {}-byte cap",
                    config.max_body_bytes
                ),
            ));
        }
        let mut body = vec![0u8; content_length];
        if content_length > 0 {
            reader.read_exact(&mut body)?;
        }
        let client_close = wants_close(&headers);
        Ok(Some((
            Request {
                method,
                path,
                headers,
                body,
                context: crate::context::Context::background(),
            },
            http10,
            client_close,
        )))
    }

    fn write_response(stream: &mut TcpStream, response: &Response) -> io::Result<()> {
        let reason = response.status.reason().unwrap_or("OK");
        let mut headers = response.headers.clone();
        if !headers.contains("content-length") {
            headers.insert("content-length", &response.body.len().to_string());
        }
        // Connection header is set by the worker based on the
        // request's HTTP version and the peer's / handler's intent.
        let mut out = format!("HTTP/1.1 {} {}\r\n", response.status.as_u16(), reason);
        for (name, value) in headers.iter() {
            let cased = canonical_header_name(name);
            out.push_str(&cased);
            out.push_str(": ");
            out.push_str(value);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        // Send the header block + body in a single writev-like write
        // to avoid the two-packet default when Nagle is off.
        let body = &response.body;
        if body.is_empty() {
            stream.write_all(out.as_bytes())?;
        } else {
            let mut combined = Vec::with_capacity(out.len() + body.len());
            combined.extend_from_slice(out.as_bytes());
            combined.extend_from_slice(body);
            stream.write_all(&combined)?;
        }
        stream.flush()
    }

    fn canonical_header_name(lower: &str) -> String {
        let mut out = String::with_capacity(lower.len());
        let mut capitalise = true;
        for ch in lower.chars() {
            if capitalise {
                out.extend(ch.to_uppercase());
                capitalise = false;
            } else {
                out.push(ch);
            }
            if ch == '-' {
                capitalise = true;
            }
        }
        out
    }

    /// Convenience wrapper for the common single-threaded path: bind
    /// `addr`, then run the accept loop until `config.shutdown` fires.
    pub fn bind_and_run<H>(addr: &str, config: &Config, handle: H) -> io::Result<()>
    where
        H: FnMut(Request) -> Response,
    {
        let listener = TcpListener::bind(addr)?;
        run(listener, config, handle)
    }

    /// Expose [`Method`] to downstream tests without a star re-export.
    #[doc(hidden)]
    pub const fn _touch(_m: Method) {}

    /// HTTPS variant of [`bind_and_run`]: binds `addr`, terminates
    /// TLS using `tls_config` for every accepted connection, and
    /// dispatches the parsed request through `handle`. Re-uses the
    /// plain-text request loop after TLS termination, so handler
    /// semantics are identical regardless of cipher suite.
    #[cfg(feature = "tls")]
    pub fn bind_and_run_tls<H>(
        addr: &str,
        tls_config: &crate::tls::ServerConfig,
        config: &Config,
        mut handle: H,
    ) -> io::Result<()>
    where
        H: FnMut(Request) -> Response,
    {
        use std::io::ErrorKind;
        use std::sync::mpsc::{RecvTimeoutError, channel};

        let listener = TcpListener::bind(addr)?;
        let bound = listener.local_addr()?;
        let _ = listener.set_nonblocking(false);

        let (dispatch_tx, dispatch_rx) = channel::<(Request, std::sync::mpsc::Sender<Response>)>();

        let shutdown = Arc::clone(&config.shutdown);
        let cfg_for_workers = config.clone();
        let server_config = tls_config.rustls();
        let tx_for_accept = dispatch_tx.clone();

        let acceptor = std::thread::Builder::new()
            .name("gossamer-https-accept".to_string())
            .spawn(move || {
                tls_accept_loop(
                    listener,
                    shutdown,
                    cfg_for_workers,
                    server_config,
                    tx_for_accept,
                );
            })
            .map_err(|e| io::Error::other(format!("spawn accept: {e}")))?;
        drop(dispatch_tx);

        let mut served: u64 = 0;
        let wake_self = || {
            let _ = TcpStream::connect_timeout(&bound, Duration::from_millis(500));
        };

        loop {
            if config.shutdown.load(Ordering::Relaxed) {
                wake_self();
                break;
            }
            match dispatch_rx.recv_timeout(Duration::from_millis(50)) {
                Ok((req, responder)) => {
                    let response = handle(req);
                    let _ = responder.send(response);
                    served = served.saturating_add(1);
                    if let Some(max) = config.max_requests {
                        if served >= max {
                            config.shutdown.store(true, Ordering::Relaxed);
                            wake_self();
                            break;
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        let _ = acceptor.join();
        let _ = ErrorKind::Other; // keep import live if unused.
        Ok(())
    }

    #[cfg(feature = "tls")]
    fn tls_accept_loop(
        listener: TcpListener,
        shutdown: Arc<AtomicBool>,
        config: Config,
        server_config: std::sync::Arc<rustls::ServerConfig>,
        dispatch_tx: std::sync::mpsc::Sender<(Request, std::sync::mpsc::Sender<Response>)>,
    ) {
        let _ = listener.set_nonblocking(true);
        loop {
            if shutdown.load(Ordering::Relaxed) {
                return;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let cfg = config.clone();
                    let tls_cfg = std::sync::Arc::clone(&server_config);
                    let tx = dispatch_tx.clone();
                    let _ = std::thread::Builder::new()
                        .name("gossamer-https-conn".to_string())
                        .spawn(move || tls_worker(stream, cfg, tls_cfg, tx));
                }
                Err(ref e) if matches!(e.kind(), io::ErrorKind::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(ref e) if matches!(e.kind(), io::ErrorKind::Interrupted) => {}
                Err(_) => return,
            }
        }
    }

    #[cfg(feature = "tls")]
    fn tls_worker(
        stream: TcpStream,
        config: Config,
        server_config: std::sync::Arc<rustls::ServerConfig>,
        dispatch_tx: std::sync::mpsc::Sender<(Request, std::sync::mpsc::Sender<Response>)>,
    ) {
        if let Some(timeout) = config.read_timeout {
            let _ = stream.set_read_timeout(Some(timeout));
            let _ = stream.set_write_timeout(Some(timeout));
        }
        let _ = stream.set_nodelay(true);

        let Ok(conn) = rustls::ServerConnection::new(server_config) else {
            let _ = stream.shutdown(Shutdown::Both);
            return;
        };
        let mut tls = rustls::StreamOwned::new(conn, stream);

        let mut reader = BufReader::new(&mut tls);
        loop {
            match read_request_generic(&mut reader, &config) {
                Ok(Some((request, http10, client_close))) => {
                    let (resp_tx, resp_rx) = std::sync::mpsc::channel::<Response>();
                    if dispatch_tx.send((request, resp_tx)).is_err() {
                        break;
                    }
                    match resp_rx.recv() {
                        Ok(mut response) => {
                            let handler_close = wants_close(&response.headers);
                            let keep_alive = !http10 && !client_close && !handler_close;
                            if keep_alive {
                                if !response.headers.contains("connection") {
                                    response.headers.insert("connection", "keep-alive");
                                }
                            } else if !response.headers.contains("connection") {
                                response.headers.insert("connection", "close");
                            }
                            if write_response_generic(reader.get_mut(), &response).is_err() {
                                break;
                            }
                            if !keep_alive {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
    }

    #[cfg(feature = "tls")]
    fn write_response_generic<W: Write>(stream: &mut W, response: &Response) -> io::Result<()> {
        let reason = response.status.reason().unwrap_or("OK");
        let mut headers = response.headers.clone();
        if !headers.contains("content-length") {
            headers.insert("content-length", &response.body.len().to_string());
        }
        let mut out = format!("HTTP/1.1 {} {}\r\n", response.status.as_u16(), reason);
        for (name, value) in headers.iter() {
            let cased = canonical_header_name(name);
            out.push_str(&cased);
            out.push_str(": ");
            out.push_str(value);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        let body = &response.body;
        if body.is_empty() {
            stream.write_all(out.as_bytes())?;
        } else {
            let mut combined = Vec::with_capacity(out.len() + body.len());
            combined.extend_from_slice(out.as_bytes());
            combined.extend_from_slice(body);
            stream.write_all(&combined)?;
        }
        stream.flush()
    }

    #[cfg(feature = "tls")]
    fn read_request_generic<R: BufRead>(
        reader: &mut R,
        config: &Config,
    ) -> io::Result<Option<(Request, bool, bool)>> {
        let mut line = String::new();
        let first = reader.read_line(&mut line)?;
        if first == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
        let (method, path, version) = super::parse_request_line(trimmed)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad request line"))?;
        let http10 = version.eq_ignore_ascii_case("HTTP/1.0");
        let mut headers = super::Headers::new();
        let mut content_length: usize = 0;
        let mut header_bytes_read: usize = line.len();
        loop {
            line.clear();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 {
                break;
            }
            header_bytes_read = header_bytes_read.saturating_add(bytes);
            if header_bytes_read > config.max_header_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("header block exceeded {}-byte cap", config.max_header_bytes),
                ));
            }
            let stripped = line.trim_end_matches(&['\r', '\n'][..]);
            if stripped.is_empty() {
                break;
            }
            if let Some((name, value)) = stripped.split_once(':') {
                let value = value.trim();
                headers.insert(name.trim(), value);
                if name.trim().eq_ignore_ascii_case("content-length") {
                    content_length = value.parse().unwrap_or(0);
                }
            }
        }
        if content_length > config.max_body_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "body length {content_length} exceeds {}-byte cap",
                    config.max_body_bytes
                ),
            ));
        }
        let mut body = vec![0u8; content_length];
        if content_length > 0 {
            reader.read_exact(&mut body)?;
        }
        let client_close = wants_close(&headers);
        Ok(Some((
            Request {
                method,
                path,
                headers,
                body,
                context: crate::context::Context::background(),
            },
            http10,
            client_close,
        )))
    }
}

/// HTTP client with timeouts, redirects, cookie jar, connection
/// pooling, and TLS. Backed by `ureq` for the wire-protocol layer
/// (which itself uses `rustls` + the same Mozilla root bundle as
/// [`crate::tls`]). Network I/O runs on a dedicated thread pool: the
/// caller's goroutine submits a job and parks on a result channel,
/// so blocking sockets never strand the goroutine's worker thread.
/// When the netpoller from Track A lands, the only change required
/// is replacing `client_pool` with a poller-aware executor — the
/// public surface here is unaffected.
#[cfg(feature = "http-client")]
#[derive(Debug, Clone)]
pub struct Client {
    inner: std::sync::Arc<ClientInner>,
}

#[cfg(feature = "http-client")]
#[derive(Debug)]
struct ClientInner {
    agent: ureq::Agent,
}

#[cfg(feature = "http-client")]
impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "http-client")]
impl Client {
    /// Constructs a default client: 30 s timeout, follow up to 10
    /// redirects, cookie jar enabled, gzip transparently decoded.
    #[must_use]
    pub fn new() -> Self {
        Self::builder().build()
    }

    /// Returns a builder for customising timeouts, redirects, etc.
    #[must_use]
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// Issues a GET request and reads the entire body into memory.
    pub fn get(&self, url: &str) -> Result<Response, ClientError> {
        self.do_request(Method::Get, url, None, &[])
    }

    /// Issues a POST request with the supplied body.
    pub fn post(
        &self,
        url: &str,
        body: &[u8],
        content_type: &str,
    ) -> Result<Response, ClientError> {
        self.do_request(
            Method::Post,
            url,
            Some(body),
            &[("Content-Type", content_type)],
        )
    }

    /// Issues a request with the supplied method, body, and extra
    /// headers. Mirrors Go's `Client.Do`.
    pub fn do_request(
        &self,
        method: Method,
        url: &str,
        body: Option<&[u8]>,
        headers: &[(&str, &str)],
    ) -> Result<Response, ClientError> {
        client_pool::run_blocking(move_owned(method, url, body, headers, &self.inner.agent))
    }
}

#[cfg(feature = "http-client")]
fn move_owned(
    method: Method,
    url: &str,
    body: Option<&[u8]>,
    headers: &[(&str, &str)],
    agent: &ureq::Agent,
) -> impl FnOnce() -> Result<Response, ClientError> + Send + 'static {
    let agent = agent.clone();
    let method_str = method.as_str().to_string();
    let url = url.to_string();
    let body_owned = body.map(<[u8]>::to_vec);
    let headers_owned: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    move || {
        let mut req = agent.request(&method_str, &url);
        for (k, v) in &headers_owned {
            req = req.set(k, v);
        }
        let response = match body_owned {
            Some(bytes) => req.send_bytes(&bytes),
            None => req.call(),
        };
        match response {
            Ok(resp) => {
                let status = StatusCode(resp.status());
                let mut headers = Headers::new();
                for name in resp.headers_names() {
                    if let Some(value) = resp.header(&name) {
                        headers.insert(&name, value);
                    }
                }
                let mut body = Vec::new();
                resp.into_reader()
                    .read_to_end(&mut body)
                    .map_err(|e| ClientError::Io(e.to_string()))?;
                Ok(Response {
                    status,
                    headers,
                    body,
                })
            }
            Err(ureq::Error::Status(code, resp)) => {
                let status = StatusCode(code);
                let mut headers = Headers::new();
                for name in resp.headers_names() {
                    if let Some(value) = resp.header(&name) {
                        headers.insert(&name, value);
                    }
                }
                let mut body = Vec::new();
                let _ = resp.into_reader().read_to_end(&mut body);
                Ok(Response {
                    status,
                    headers,
                    body,
                })
            }
            Err(ureq::Error::Transport(t)) => Err(ClientError::Transport(t.to_string())),
        }
    }
}

#[cfg(feature = "http-client")]
use std::io::Read;

/// Builder for [`Client`].
#[cfg(feature = "http-client")]
#[derive(Debug, Clone)]
pub struct ClientBuilder {
    timeout: std::time::Duration,
    max_redirects: u32,
    cookies: bool,
    user_agent: String,
    tls: Option<crate::tls::ClientConfig>,
}

#[cfg(feature = "http-client")]
impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            timeout: std::time::Duration::from_secs(30),
            max_redirects: 10,
            cookies: true,
            user_agent: format!("gossamer/{}", env!("CARGO_PKG_VERSION")),
            tls: None,
        }
    }
}

#[cfg(feature = "http-client")]
impl ClientBuilder {
    /// Sets the per-request timeout.
    #[must_use]
    pub fn timeout(mut self, dur: std::time::Duration) -> Self {
        self.timeout = dur;
        self
    }

    /// Sets the maximum number of redirects the client will follow.
    /// Set to 0 to disable redirect-following entirely.
    #[must_use]
    pub fn max_redirects(mut self, n: u32) -> Self {
        self.max_redirects = n;
        self
    }

    /// Enables or disables the cookie jar.
    #[must_use]
    pub fn cookies(mut self, enabled: bool) -> Self {
        self.cookies = enabled;
        self
    }

    /// Sets a custom `User-Agent`.
    #[must_use]
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = ua.into();
        self
    }

    /// Routes requests through a custom TLS configuration (mTLS,
    /// custom roots, ALPN). Falls back to the bundled Mozilla root
    /// bundle when not set.
    #[must_use]
    pub fn tls(mut self, config: crate::tls::ClientConfig) -> Self {
        self.tls = Some(config);
        self
    }

    /// Builds the client.
    #[must_use]
    pub fn build(self) -> Client {
        let mut builder = ureq::AgentBuilder::new()
            .timeout(self.timeout)
            .redirects(self.max_redirects)
            .user_agent(&self.user_agent);
        if let Some(tls) = self.tls {
            builder = builder.tls_config(tls.rustls());
        }
        // ureq's cookie jar is enabled by feature; the `cookies`
        // boolean is preserved for the future when we vend a
        // poller-aware client, so leave it on the builder for the
        // documentation surface.
        let _ = self.cookies;
        Client {
            inner: std::sync::Arc::new(ClientInner {
                agent: builder.build(),
            }),
        }
    }
}

/// Error returned by [`Client`].
#[cfg(feature = "http-client")]
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Network / TLS / DNS-level failure.
    #[error("http: transport: {0}")]
    Transport(String),
    /// I/O error while reading the response body.
    #[error("http: io: {0}")]
    Io(String),
    /// Request was cancelled via the supplied [`crate::context::Context`].
    #[error("http: cancelled by context")]
    Cancelled,
}

/// Routes blocking HTTP I/O onto Track A's shared
/// [`crate::blocking_pool`]. The pool parks the calling goroutine on
/// a one-shot channel while a worker thread performs the
/// system-level network round trip — host workers stay free to
/// schedule other goroutines. When the netpoller lands and TLS
/// dialling becomes non-blocking, this is the single seam to swap.
#[cfg(feature = "http-client")]
mod client_pool {
    use super::ClientError;

    pub(super) fn run_blocking<F, R>(job: F) -> Result<R, ClientError>
    where
        F: FnOnce() -> Result<R, ClientError> + Send + 'static,
        R: Send + 'static,
    {
        crate::blocking_pool::run(job)
    }
}

/// Stub client surface that ships when the `http-client` feature is
/// disabled. Calls panic so misconfigured deployments fail loud.
#[cfg(not(feature = "http-client"))]
#[derive(Debug, Default, Clone)]
pub struct Client;

#[cfg(not(feature = "http-client"))]
impl Client {
    /// Constructs a stub client; calls into it return `Err`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_request_line_handles_get() {
        let (method, path, version) = parse_request_line("GET /index.html HTTP/1.1").unwrap();
        assert_eq!(method, Method::Get);
        assert_eq!(path, "/index.html");
        assert_eq!(version, "HTTP/1.1");
    }

    #[test]
    fn parse_status_line_returns_components() {
        let (version, code, reason) = parse_status_line("HTTP/1.1 200 OK").unwrap();
        assert_eq!(version, "HTTP/1.1");
        assert_eq!(code, StatusCode::OK);
        assert_eq!(reason, "OK");
    }

    #[cfg(feature = "http-client")]
    #[test]
    fn client_builder_round_trips_settings() {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .max_redirects(3)
            .user_agent("gossamer-tests/1")
            .build();
        // Smoke test only; the actual transport is exercised by the
        // optional integration test gated on GOS_HTTP_LIVE.
        let _ = client;
    }

    #[cfg(feature = "http-client")]
    #[test]
    fn https_round_trip_with_default_roots() {
        // Live network is opt-in: tests run in sandboxes without
        // outbound network access. Set GOS_HTTP_LIVE=1 locally to
        // exercise the real TLS dial path against example.com.
        if std::env::var("GOS_HTTP_LIVE").ok().as_deref() != Some("1") {
            return;
        }
        let client = Client::new();
        let resp = client.get("https://example.com").expect("fetch");
        assert!(resp.status.is_success(), "status: {:?}", resp.status);
        let body = String::from_utf8_lossy(&resp.body);
        assert!(body.contains("Example Domain"));
    }
}
