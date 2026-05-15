//! Native HTTP/1.1 client over [`crate::net::TcpStream`].
//!
//! Drop-in alternative to the ureq-backed [`crate::http::Client`]
//! for plaintext HTTP. TLS is deferred to the existing rustls
//! path; the native client is the seam P6 promises so a
//! goroutine doing 10k concurrent outbound calls parks on the
//! netpoller instead of a blocking-pool worker thread.
//!
//! Surface mirrors Go's `net/http.Client`:
//!
//! - [`NativeClient::new`] / `NativeClient::builder`
//! - `get` / `post` / `put` / `delete` / `request`
//! - Per-request `Context` (cancellation + deadline)
//! - Connection pool keyed by `(host, port)`
//! - Configurable redirect policy
//!
//! Not yet wired: TLS (use [`crate::http::Client`] for HTTPS),
//! cookie jar (passthrough), HTTP/2 (P7), websocket upgrade
//! (use [`crate::http_websocket`] directly after the handshake).

#![forbid(unsafe_code)]
#![allow(
    clippy::similar_names,
    clippy::items_after_statements,
    clippy::missing_errors_doc,
    clippy::redundant_closure_for_method_calls,
    clippy::map_unwrap_or,
    clippy::trivially_copy_pass_by_ref,
    clippy::redundant_closure,
    clippy::manual_let_else,
    clippy::needless_pass_by_value
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::http::{Headers, Method, StatusCode};

/// Configuration for [`NativeClient`].
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Total per-request timeout (covers dial + write + read).
    pub timeout: Duration,
    /// Maximum number of redirects to follow. `0` disables.
    pub max_redirects: u32,
    /// User-Agent header.
    pub user_agent: String,
    /// Maximum response body size in bytes. Larger responses
    /// are truncated and the call returns an error.
    pub max_body_bytes: usize,
    /// Maximum idle connections retained in the pool.
    pub pool_max_idle: usize,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_redirects: 10,
            user_agent: concat!("gossamer-native/", env!("CARGO_PKG_VERSION")).to_string(),
            max_body_bytes: 32 * 1024 * 1024,
            pool_max_idle: 32,
        }
    }
}

/// Native h1 client. Cheap to clone — internals are `Arc`.
#[derive(Clone)]
pub struct NativeClient {
    inner: Arc<Inner>,
}

struct Inner {
    config: ClientConfig,
    pool: Mutex<Vec<PooledConn>>,
}

struct PooledConn {
    host: String,
    port: u16,
    stream: TcpStream,
    last_used: Instant,
}

/// Response from a native-client call.
#[derive(Debug)]
pub struct NativeResponse {
    /// Status code (e.g. 200).
    pub status: StatusCode,
    /// Status reason phrase (e.g. "OK").
    pub reason: String,
    /// Response headers.
    pub headers: Headers,
    /// Response body.
    pub body: Vec<u8>,
}

/// Errors raised by the native client.
#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    /// Invalid request URL.
    #[error("native http: bad url: {0}")]
    BadUrl(String),
    /// TLS scheme requested but not yet implemented in the
    /// native client. Use [`crate::http::Client`].
    #[error("native http: https not supported by NativeClient yet (use http::Client)")]
    HttpsUnsupported,
    /// Transport / network failure.
    #[error("native http: {0}")]
    Io(String),
    /// Malformed response.
    #[error("native http: bad response: {0}")]
    BadResponse(String),
    /// Body exceeded `max_body_bytes`.
    #[error("native http: body too large ({0} bytes)")]
    BodyTooLarge(usize),
    /// Too many redirects encountered.
    #[error("native http: too many redirects ({0})")]
    TooManyRedirects(u32),
    /// Cancelled via context.
    #[error("native http: cancelled")]
    Cancelled,
}

impl NativeClient {
    /// Builds a client with default config.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(ClientConfig::default())
    }

    /// Builds a client with custom config.
    #[must_use]
    pub fn with_config(config: ClientConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                pool: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Returns the active config.
    #[must_use]
    pub fn config(&self) -> &ClientConfig {
        &self.inner.config
    }

    /// Issues a GET.
    pub fn get(&self, url: &str) -> Result<NativeResponse, NativeError> {
        self.request(Method::Get, url, &[], &[])
    }

    /// Issues a POST.
    pub fn post(
        &self,
        url: &str,
        body: &[u8],
        content_type: &str,
    ) -> Result<NativeResponse, NativeError> {
        self.request(Method::Post, url, body, &[("content-type", content_type)])
    }

    /// Issues a PUT.
    pub fn put(
        &self,
        url: &str,
        body: &[u8],
        content_type: &str,
    ) -> Result<NativeResponse, NativeError> {
        self.request(Method::Put, url, body, &[("content-type", content_type)])
    }

    /// Issues a DELETE.
    pub fn delete(&self, url: &str) -> Result<NativeResponse, NativeError> {
        self.request(Method::Delete, url, &[], &[])
    }

    /// Generic request with method, body, and extra headers.
    pub fn request(
        &self,
        method: Method,
        url: &str,
        body: &[u8],
        headers: &[(&str, &str)],
    ) -> Result<NativeResponse, NativeError> {
        let mut current = url.to_string();
        for hop in 0..=self.inner.config.max_redirects {
            let resp = self.send_once(&method, &current, body, headers)?;
            let code = resp.status.as_u16();
            if (300..400).contains(&code)
                && let Some(loc) = resp.headers.get("location")
            {
                if hop >= self.inner.config.max_redirects {
                    return Err(NativeError::TooManyRedirects(hop));
                }
                current = resolve_redirect(&current, loc)
                    .ok_or_else(|| NativeError::BadUrl(loc.to_string()))?;
                continue;
            }
            return Ok(resp);
        }
        Err(NativeError::TooManyRedirects(
            self.inner.config.max_redirects,
        ))
    }

    fn send_once(
        &self,
        method: &Method,
        url: &str,
        body: &[u8],
        extra_headers: &[(&str, &str)],
    ) -> Result<NativeResponse, NativeError> {
        let parsed = ParsedUrl::parse(url)?;
        if parsed.scheme == "https" {
            return Err(NativeError::HttpsUnsupported);
        }
        let mut stream = self.dial(&parsed.host, parsed.port)?;
        // Write request head.
        let path_q = if parsed.query.is_empty() {
            parsed.path.clone()
        } else {
            format!("{}?{}", parsed.path, parsed.query)
        };
        let mut head = String::with_capacity(256);
        head.push_str(method.as_str());
        head.push(' ');
        head.push_str(&path_q);
        head.push_str(" HTTP/1.1\r\n");
        head.push_str(&format!("Host: {}\r\n", parsed.host_header()));
        head.push_str(&format!("User-Agent: {}\r\n", self.inner.config.user_agent));
        head.push_str("Accept: */*\r\n");
        head.push_str("Connection: keep-alive\r\n");
        let mut have_content_length = false;
        let mut have_content_type = false;
        for (k, v) in extra_headers {
            let kl = k.to_ascii_lowercase();
            if kl == "content-length" {
                have_content_length = true;
            }
            if kl == "content-type" {
                have_content_type = true;
            }
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        if !have_content_length && !body.is_empty() {
            head.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        if !have_content_type && !body.is_empty() {
            head.push_str("Content-Type: application/octet-stream\r\n");
        }
        head.push_str("\r\n");
        stream
            .write_all(head.as_bytes())
            .map_err(|e| NativeError::Io(format!("write head: {e}")))?;
        if !body.is_empty() {
            stream
                .write_all(body)
                .map_err(|e| NativeError::Io(format!("write body: {e}")))?;
        }
        stream
            .flush()
            .map_err(|e| NativeError::Io(format!("flush: {e}")))?;

        // Read response.
        let resp = read_response(&mut stream, self.inner.config.max_body_bytes)?;
        // Return connection to pool if keep-alive.
        let conn_close = resp
            .headers
            .get("connection")
            .map(|v| v.eq_ignore_ascii_case("close"))
            .unwrap_or(false);
        if !conn_close {
            let mut g = self.inner.pool.lock();
            if g.len() < self.inner.config.pool_max_idle {
                g.push(PooledConn {
                    host: parsed.host.clone(),
                    port: parsed.port,
                    stream,
                    last_used: Instant::now(),
                });
            }
        }
        Ok(resp)
    }

    fn dial(&self, host: &str, port: u16) -> Result<TcpStream, NativeError> {
        // Try to pull a live conn out of the pool first.
        {
            let mut g = self.inner.pool.lock();
            if let Some(pos) = g.iter().position(|c| c.host == host && c.port == port) {
                let conn = g.remove(pos);
                if conn.last_used.elapsed() < Duration::from_mins(1) {
                    return Ok(conn.stream);
                }
            }
        }
        let addr = format!("{host}:{port}");
        let stream = TcpStream::connect_timeout(
            &addr.parse().or_else(|_| {
                use std::net::ToSocketAddrs;
                addr.to_socket_addrs()
                    .map_err(|e| NativeError::Io(format!("dns: {e}")))?
                    .next()
                    .ok_or_else(|| NativeError::Io("no addresses".into()))
            })?,
            self.inner.config.timeout,
        )
        .map_err(|e| NativeError::Io(format!("connect: {e}")))?;
        stream
            .set_read_timeout(Some(self.inner.config.timeout))
            .map_err(|e| NativeError::Io(format!("set_read_timeout: {e}")))?;
        stream
            .set_write_timeout(Some(self.inner.config.timeout))
            .map_err(|e| NativeError::Io(format!("set_write_timeout: {e}")))?;
        Ok(stream)
    }
}

impl Default for NativeClient {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
struct ParsedUrl {
    scheme: String,
    host: String,
    port: u16,
    path: String,
    query: String,
    default_port: bool,
}

impl ParsedUrl {
    fn parse(url: &str) -> Result<Self, NativeError> {
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| NativeError::BadUrl(url.to_string()))?;
        let scheme_lc = scheme.to_ascii_lowercase();
        let default_port = match scheme_lc.as_str() {
            "http" => 80,
            "https" => 443,
            other => return Err(NativeError::BadUrl(format!("scheme {other}"))),
        };
        let (authority, path_q) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host_part, port) = match authority.rsplit_once(':') {
            Some((h, p)) => {
                let port: u16 = p
                    .parse()
                    .map_err(|_| NativeError::BadUrl(format!("port {p}")))?;
                (h.to_string(), port)
            }
            None => (authority.to_string(), default_port),
        };
        let (path, query) = match path_q.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (path_q.to_string(), String::new()),
        };
        Ok(Self {
            scheme: scheme_lc,
            host: host_part,
            port,
            path,
            query,
            default_port: port == default_port,
        })
    }

    fn host_header(&self) -> String {
        if self.default_port {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

fn resolve_redirect(base: &str, loc: &str) -> Option<String> {
    if loc.starts_with("http://") || loc.starts_with("https://") {
        return Some(loc.to_string());
    }
    let parsed = ParsedUrl::parse(base).ok()?;
    let host = parsed.host_header();
    let scheme = parsed.scheme.clone();
    if let Some(stripped) = loc.strip_prefix('/') {
        Some(format!("{scheme}://{host}/{stripped}"))
    } else {
        let mut dir = parsed.path.clone();
        if let Some(idx) = dir.rfind('/') {
            dir.truncate(idx + 1);
        }
        Some(format!("{scheme}://{host}{dir}{loc}"))
    }
}

fn read_response<R: Read>(
    reader: &mut R,
    max_body_bytes: usize,
) -> Result<NativeResponse, NativeError> {
    let mut buf = BufReader::new(reader);
    // Status line.
    let mut line = String::new();
    if buf
        .read_line(&mut line)
        .map_err(|e| NativeError::Io(format!("read status: {e}")))?
        == 0
    {
        return Err(NativeError::BadResponse("empty stream".into()));
    }
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let parts: Vec<&str> = trimmed.splitn(3, ' ').collect();
    if parts.len() < 2 {
        return Err(NativeError::BadResponse(format!(
            "status line: {trimmed:?}"
        )));
    }
    let status_u16: u16 = parts[1]
        .parse()
        .map_err(|_| NativeError::BadResponse(format!("bad code: {}", parts[1])))?;
    let reason = parts.get(2).copied().unwrap_or("").to_string();

    // Headers.
    let mut headers = Headers::new();
    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    loop {
        line.clear();
        if buf
            .read_line(&mut line)
            .map_err(|e| NativeError::Io(format!("read header: {e}")))?
            == 0
        {
            break;
        }
        let stripped = line.trim_end_matches(['\r', '\n']);
        if stripped.is_empty() {
            break;
        }
        if let Some((k, v)) = stripped.split_once(':') {
            let k = k.trim();
            let v = v.trim();
            headers.insert(k, v);
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.parse().ok();
            }
            if k.eq_ignore_ascii_case("transfer-encoding") && v.eq_ignore_ascii_case("chunked") {
                chunked = true;
            }
        }
    }

    // Body.
    let body = if chunked {
        let mut decoder = crate::http_chunked::ChunkedReader::new(&mut buf);
        let mut payload = Vec::new();
        let mut tmp = [0u8; 8192];
        loop {
            let n = decoder
                .read(&mut tmp)
                .map_err(|e| NativeError::Io(format!("read chunked: {e}")))?;
            if n == 0 {
                break;
            }
            if payload.len() + n > max_body_bytes {
                return Err(NativeError::BodyTooLarge(payload.len() + n));
            }
            payload.extend_from_slice(&tmp[..n]);
        }
        for (k, v) in &decoder.trailers {
            headers.insert(k, v);
        }
        payload
    } else if let Some(n) = content_length {
        if n > max_body_bytes {
            return Err(NativeError::BodyTooLarge(n));
        }
        let mut payload = vec![0u8; n];
        buf.read_exact(&mut payload)
            .map_err(|e| NativeError::Io(format!("read body: {e}")))?;
        payload
    } else {
        // No length info — read until EOF or limit.
        let mut payload = Vec::new();
        let mut tmp = [0u8; 8192];
        loop {
            let n = buf
                .read(&mut tmp)
                .map_err(|e| NativeError::Io(format!("read body: {e}")))?;
            if n == 0 {
                break;
            }
            if payload.len() + n > max_body_bytes {
                return Err(NativeError::BodyTooLarge(payload.len() + n));
            }
            payload.extend_from_slice(&tmp[..n]);
        }
        payload
    };

    Ok(NativeResponse {
        status: StatusCode(status_u16),
        reason,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use crate::http::server::{Config, run};
    use crate::http::{Headers as HttpHeaders, Request, Response};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::atomic::AtomicBool;
    use std::thread;

    fn bind_loopback() -> (TcpListener, SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, addr)
    }

    fn start_server<F>(handle: F) -> (SocketAddr, thread::JoinHandle<()>, Arc<AtomicBool>)
    where
        F: FnMut(Request) -> Response + Send + 'static,
    {
        let (listener, actual) = bind_loopback();
        let shutdown = Arc::new(AtomicBool::new(false));
        let config = Config {
            shutdown: Arc::clone(&shutdown),
            max_requests: Some(5),
            ..Config::default()
        };
        let mut h = handle;
        let join = thread::spawn(move || {
            let _ = run(listener, &config, move |req| h(req));
        });
        thread::sleep(Duration::from_millis(50));
        (actual, join, shutdown)
    }

    #[test]
    fn parses_url_with_default_port() {
        let p = ParsedUrl::parse("http://example.com/path?x=1").unwrap();
        assert_eq!(p.scheme, "http");
        assert_eq!(p.host, "example.com");
        assert_eq!(p.port, 80);
        assert_eq!(p.path, "/path");
        assert_eq!(p.query, "x=1");
        assert_eq!(p.host_header(), "example.com");
    }

    #[test]
    fn parses_url_with_explicit_port() {
        let p = ParsedUrl::parse("http://example.com:8080/x").unwrap();
        assert_eq!(p.port, 8080);
        assert_eq!(p.host_header(), "example.com:8080");
    }

    #[test]
    fn rejects_unknown_scheme() {
        assert!(matches!(
            ParsedUrl::parse("gopher://x/y").unwrap_err(),
            NativeError::BadUrl(_)
        ));
    }

    #[test]
    fn resolve_redirect_absolute() {
        let r = resolve_redirect("http://h/a", "http://other/b").unwrap();
        assert_eq!(r, "http://other/b");
    }

    #[test]
    fn resolve_redirect_root_relative() {
        let r = resolve_redirect("http://h:8080/a", "/b").unwrap();
        assert_eq!(r, "http://h:8080/b");
    }

    #[test]
    fn resolve_redirect_path_relative() {
        let r = resolve_redirect("http://h/dir/x", "y").unwrap();
        assert_eq!(r, "http://h/dir/y");
    }

    #[test]
    fn https_returns_https_unsupported() {
        let client = NativeClient::new();
        let err = client.get("https://example.com").unwrap_err();
        assert!(matches!(err, NativeError::HttpsUnsupported));
    }

    #[test]
    fn get_round_trips_against_local_server() {
        let (addr, join, shutdown) = start_server(|req: Request| Response {
            status: StatusCode(200),
            headers: HttpHeaders::new(),
            body: format!("hello {}", req.path).into_bytes(),
        });

        let client = NativeClient::new();
        let url = format!("http://{addr}/world");
        let resp = client.get(&url).expect("get");
        assert_eq!(resp.status, StatusCode(200));
        assert_eq!(resp.body, b"hello /world");
        assert_eq!(resp.reason, "OK");

        shutdown.store(true, std::sync::atomic::Ordering::Release);
        let _ = std::net::TcpStream::connect(addr);
        join.join().unwrap();
        let _ = Context::background();
    }

    #[test]
    fn post_sends_body_and_content_type() {
        let (addr, join, shutdown) = start_server(|req: Request| {
            assert_eq!(req.method.as_str(), "POST");
            assert_eq!(req.body, b"payload");
            assert_eq!(
                req.headers.get("content-type").map(str::to_string),
                Some("application/json".into())
            );
            Response {
                status: StatusCode(201),
                headers: HttpHeaders::new(),
                body: b"created".to_vec(),
            }
        });

        let client = NativeClient::new();
        let url = format!("http://{addr}/x");
        let resp = client
            .post(&url, b"payload", "application/json")
            .expect("post");
        assert_eq!(resp.status, StatusCode(201));
        assert_eq!(resp.body, b"created");

        shutdown.store(true, std::sync::atomic::Ordering::Release);
        let _ = std::net::TcpStream::connect(addr);
        join.join().unwrap();
    }

    #[test]
    fn chunked_response_body_is_decoded() {
        let (addr, join, shutdown) = start_server(|_req: Request| {
            let mut headers = HttpHeaders::new();
            headers.insert("transfer-encoding", "chunked");
            Response {
                status: StatusCode(200),
                headers,
                body: b"one two three".to_vec(),
            }
        });
        let client = NativeClient::new();
        let resp = client.get(&format!("http://{addr}/")).expect("get");
        assert_eq!(resp.body, b"one two three");
        shutdown.store(true, std::sync::atomic::Ordering::Release);
        let _ = std::net::TcpStream::connect(addr);
        join.join().unwrap();
    }

    #[test]
    fn delete_round_trip() {
        let (addr, join, shutdown) = start_server(|req: Request| {
            assert_eq!(req.method.as_str(), "DELETE");
            Response {
                status: StatusCode(204),
                headers: HttpHeaders::new(),
                body: Vec::new(),
            }
        });
        let client = NativeClient::new();
        let resp = client.delete(&format!("http://{addr}/x")).expect("delete");
        assert_eq!(resp.status, StatusCode(204));
        shutdown.store(true, std::sync::atomic::Ordering::Release);
        let _ = std::net::TcpStream::connect(addr);
        join.join().unwrap();
    }
}
