//! HTTP(S) transport for the package fetcher.
//! Two real implementations plus a test double:
//! - [`HttpTransport`] - plain `http://` over [`std::net::TcpStream`].
//! - [`HttpsTransport`] - `https://` over a `rustls` client session
//!   pinned to the Mozilla-maintained root CAs from `webpki-roots`.
//! - [`StaticTransport`] - in-memory URL → bytes map. Used by tests
//!   and by the registry resolver's synthetic path.
//!
//! Downloaded bytes are always paired with a SHA-256 digest that the
//! fetcher compares against the expected `sha256 = ...` field from
//! the project manifest before admitting the payload into the cache.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use rustls::client::ClientConnection;
use rustls::{ClientConfig, RootCertStore, Stream};
use rustls_pki_types::ServerName;
use url::{Host, Url};

use crate::sha256;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_HEADER_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Error shape for transport failures.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum TransportError {
    /// URL did not parse into a scheme+host+path triple we handle.
    #[error("bad url: {0}")]
    BadUrl(String),
    /// A plaintext `http://` URL was used for a non-loopback host
    /// without the insecure-registry opt-in. Refused so registry
    /// traffic (index, downloads) is not silently downgraded off TLS.
    #[error(
        "refusing plaintext http for {0}: set GOS_ALLOW_INSECURE_REGISTRY=1 to allow an insecure registry"
    )]
    InsecureScheme(String),
    /// The HTTPS scheme was requested but the transport used cannot
    /// speak TLS.
    #[error("https not supported by transport")]
    HttpsUnsupported,
    /// Network I/O failure.
    #[error("io: {0}")]
    Io(String),
    /// The server returned a non-2xx response.
    #[error("http status {status}: {reason}")]
    BadStatus {
        /// HTTP numeric status.
        status: u16,
        /// Reason phrase from the response.
        reason: String,
    },
    /// The response body hashed to something other than the pinned
    /// digest.
    #[error("digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch {
        /// Hex digest the caller asked for.
        expected: String,
        /// Hex digest the response actually hashed to.
        actual: String,
    },
    /// A response exceeded the transport's bounded in-memory limit.
    #[error("response exceeds {limit}-byte limit")]
    ResponseTooLarge {
        /// Maximum permitted response size.
        limit: usize,
    },
}

/// Abstract transport the fetcher drives.
pub trait Transport: Send + Sync {
    /// Fetches the body at `url`. Returns the raw bytes, without
    /// interpreting Content-Type.
    ///
    /// # Errors
    ///
    /// Returns a [`TransportError`] when DNS / TCP / TLS / status-line
    /// parsing fails.
    fn get(&self, url: &str) -> Result<Vec<u8>, TransportError>;

    /// Streams a response body to `out`. Implementations should avoid
    /// materialising the response before writing it. The default preserves
    /// compatibility for existing transports while allowing new callers to
    /// use a reader/writer-oriented API.
    fn get_to_writer(&self, url: &str, out: &mut dyn Write) -> Result<(), TransportError> {
        let body = self.get(url)?;
        out.write_all(&body)
            .map_err(|e| TransportError::Io(format!("write response: {e}")))
    }

    /// Sends `body` to `url` with the given `content_type` and an
    /// optional `Authorization: Bearer` token. Returns the
    /// response body bytes on a 2xx response. Default
    /// implementation returns [`TransportError::HttpsUnsupported`]
    /// so existing read-only transports (the in-memory test
    /// double, the empty default) don't have to opt in.
    ///
    /// # Errors
    ///
    /// Returns a [`TransportError`] when DNS / TCP / TLS / status-line
    /// parsing fails. Non-2xx response status surfaces as
    /// [`TransportError::BadStatus`].
    fn post(
        &self,
        url: &str,
        body: &[u8],
        content_type: &str,
        auth_token: Option<&str>,
    ) -> Result<Vec<u8>, TransportError> {
        let _ = (url, body, content_type, auth_token);
        Err(TransportError::Io(
            "transport does not support POST".to_string(),
        ))
    }

    /// Streams a request body whose exact length is known. The default keeps
    /// older transports source-compatible; network transports override it so
    /// publishing does not require a second full request allocation.
    fn post_reader(
        &self,
        url: &str,
        body: &mut dyn Read,
        body_len: usize,
        content_type: &str,
        auth_token: Option<&str>,
    ) -> Result<Vec<u8>, TransportError> {
        if body_len > MAX_RESPONSE_BYTES {
            return Err(TransportError::ResponseTooLarge {
                limit: MAX_RESPONSE_BYTES,
            });
        }
        let mut bytes = Vec::with_capacity(body_len);
        body.take((body_len as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| TransportError::Io(format!("read request: {e}")))?;
        if bytes.len() != body_len {
            return Err(TransportError::Io(
                "request body length does not match declared length".to_string(),
            ));
        }
        self.post(url, &bytes, content_type, auth_token)
    }

    /// Streams a fixed-size request with protocol-specific, validated HTTP
    /// headers. This exists for package publication: archive metadata belongs
    /// in headers while the archive itself remains a reader, rather than being
    /// expanded into a JSON/base16 allocation.
    fn post_reader_with_headers(
        &self,
        url: &str,
        body: &mut dyn Read,
        body_len: usize,
        content_type: &str,
        auth_token: Option<&str>,
        headers: &[(&str, &str)],
    ) -> Result<Vec<u8>, TransportError> {
        if !headers.is_empty() {
            return Err(TransportError::Io(
                "transport does not support protocol request headers".to_string(),
            ));
        }
        self.post_reader(url, body, body_len, content_type, auth_token)
    }
}

/// Parsed URL slices.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedUrl {
    scheme: String,
    host: String,
    port: u16,
    path: String,
}

fn parse_url(url: &str) -> Result<ParsedUrl, TransportError> {
    if url.is_empty() || url.len() > 8 * 1024 || has_header_control(url) || url.contains('\\') {
        return Err(TransportError::BadUrl(url.to_string()));
    }
    let parsed = Url::parse(url).map_err(|_| TransportError::BadUrl(url.to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.fragment().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(TransportError::BadUrl(url.to_string()));
    }
    let host = match parsed.host() {
        Some(Host::Domain(host)) => host.to_string(),
        Some(Host::Ipv4(host)) => host.to_string(),
        Some(Host::Ipv6(host)) => host.to_string(),
        None => return Err(TransportError::BadUrl(url.to_string())),
    };
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| TransportError::BadUrl(url.to_string()))?;
    let mut path = parsed.path().to_string();
    if path.is_empty() {
        path.push('/');
    }
    if let Some(query) = parsed.query() {
        path.push('?');
        path.push_str(query);
    }
    Ok(ParsedUrl {
        scheme: parsed.scheme().to_string(),
        host,
        port,
        path,
    })
}

impl ParsedUrl {
    fn host_header(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let default_port = if self.scheme == "https" { 443 } else { 80 };
        if self.port == default_port {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

fn has_header_control(text: &str) -> bool {
    text.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0)
}

fn connect(parsed: &ParsedUrl) -> Result<TcpStream, TransportError> {
    let mut addrs = (parsed.host.as_str(), parsed.port)
        .to_socket_addrs()
        .map_err(|e| TransportError::Io(format!("resolve {}:{}: {e}", parsed.host, parsed.port)))?;
    let addr = addrs.next().ok_or_else(|| {
        TransportError::Io(format!(
            "resolve {}:{}: no addresses",
            parsed.host, parsed.port
        ))
    })?;
    TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|e| TransportError::Io(format!("connect {addr}: {e}")))
}

fn configure_socket(stream: &TcpStream) -> Result<(), TransportError> {
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|e| TransportError::Io(format!("read_timeout: {e}")))?;
    stream
        .set_write_timeout(Some(READ_TIMEOUT))
        .map_err(|e| TransportError::Io(format!("write_timeout: {e}")))
}

/// Plain-HTTP transport. Refuses `https://` URLs.
#[derive(Debug, Default, Clone)]
pub struct HttpTransport;

impl Transport for HttpTransport {
    fn get(&self, url: &str) -> Result<Vec<u8>, TransportError> {
        let parsed = parse_url(url)?;
        if parsed.scheme != "http" {
            return Err(TransportError::HttpsUnsupported);
        }
        let mut stream = connect(&parsed)?;
        configure_socket(&stream)?;
        write_http_request(&mut stream, &parsed)?;
        let response = read_entire(&mut stream, MAX_RESPONSE_BYTES)?;
        parse_http_response(response)
    }

    fn post(
        &self,
        url: &str,
        body: &[u8],
        content_type: &str,
        auth_token: Option<&str>,
    ) -> Result<Vec<u8>, TransportError> {
        let parsed = parse_url(url)?;
        if parsed.scheme != "http" {
            return Err(TransportError::HttpsUnsupported);
        }
        let mut stream = connect(&parsed)?;
        configure_socket(&stream)?;
        write_http_post(&mut stream, &parsed, body, content_type, auth_token)?;
        let response = read_entire(&mut stream, MAX_RESPONSE_BYTES)?;
        parse_http_response(response)
    }

    fn get_to_writer(&self, url: &str, out: &mut dyn Write) -> Result<(), TransportError> {
        let parsed = parse_url(url)?;
        if parsed.scheme != "http" {
            return Err(TransportError::HttpsUnsupported);
        }
        let mut stream = connect(&parsed)?;
        configure_socket(&stream)?;
        write_http_request(&mut stream, &parsed)?;
        read_http_response_to_writer(&mut stream, out)
    }

    fn post_reader(
        &self,
        url: &str,
        body: &mut dyn Read,
        body_len: usize,
        content_type: &str,
        auth_token: Option<&str>,
    ) -> Result<Vec<u8>, TransportError> {
        let parsed = parse_url(url)?;
        if parsed.scheme != "http" {
            return Err(TransportError::HttpsUnsupported);
        }
        let mut stream = connect(&parsed)?;
        configure_socket(&stream)?;
        write_http_post_reader(
            &mut stream,
            &parsed,
            body,
            body_len,
            content_type,
            auth_token,
        )?;
        let mut response = Vec::new();
        read_http_response_to_writer(&mut stream, &mut response)?;
        Ok(response)
    }

    fn post_reader_with_headers(
        &self,
        url: &str,
        body: &mut dyn Read,
        body_len: usize,
        content_type: &str,
        auth_token: Option<&str>,
        headers: &[(&str, &str)],
    ) -> Result<Vec<u8>, TransportError> {
        let parsed = parse_url(url)?;
        if parsed.scheme != "http" {
            return Err(TransportError::HttpsUnsupported);
        }
        let mut stream = connect(&parsed)?;
        configure_socket(&stream)?;
        write_http_post_reader_with_headers(
            &mut stream,
            &parsed,
            body,
            body_len,
            content_type,
            auth_token,
            headers,
        )?;
        let mut response = Vec::new();
        read_http_response_to_writer(&mut stream, &mut response)?;
        Ok(response)
    }
}

/// Rustls-backed HTTPS transport, pinned to the Mozilla CA bundle.
pub struct HttpsTransport {
    config: Arc<ClientConfig>,
    /// When `true`, a plaintext `http://` URL to a non-loopback host is
    /// allowed (the `--allow-insecure-registry` / env opt-in). Default
    /// `false`: such URLs are refused rather than silently downgraded.
    allow_insecure: bool,
}

impl HttpsTransport {
    /// Constructs a transport configured with the bundled Mozilla
    /// root CA store.
    ///
    /// # Panics
    ///
    /// Panics if `rustls::crypto::ring::default_provider().install_default()`
    /// has already been called with a different provider; gossamer
    /// installs `ring` unconditionally.
    #[must_use]
    pub fn new_mozilla_roots() -> Self {
        Self::new_mozilla_roots_with_insecure(false)
    }

    /// Like [`Self::new_mozilla_roots`] but permits plaintext `http://`
    /// to non-loopback hosts. Use only for a trusted dev/internal
    /// registry; production registries must be `https://`.
    #[must_use]
    pub fn new_mozilla_roots_insecure() -> Self {
        Self::new_mozilla_roots_with_insecure(true)
    }

    fn new_mozilla_roots_with_insecure(allow_insecure: bool) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Self {
            config: Arc::new(config),
            allow_insecure,
        }
    }

    /// Whether a plaintext `http://` request to `host` is permitted:
    /// loopback hosts are always safe (no off-host attacker), otherwise
    /// only under the insecure opt-in.
    fn http_allowed(&self, host: &str) -> bool {
        self.allow_insecure || is_loopback_host(host)
    }
}

/// Returns `true` for loopback hosts (`localhost`, `127.0.0.0/8`,
/// `::1`), which cannot be intercepted by an off-host network attacker.
fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

impl Transport for HttpsTransport {
    fn get(&self, url: &str) -> Result<Vec<u8>, TransportError> {
        let parsed = parse_url(url)?;
        if parsed.scheme == "http" {
            if self.http_allowed(&parsed.host) {
                return HttpTransport.get(url);
            }
            return Err(TransportError::InsecureScheme(url.to_string()));
        }
        if parsed.scheme != "https" {
            return Err(TransportError::BadUrl(url.to_string()));
        }
        let server_name = ServerName::try_from(parsed.host.clone())
            .map_err(|e| TransportError::BadUrl(format!("{url}: {e}")))?;
        let mut client = ClientConnection::new(Arc::clone(&self.config), server_name)
            .map_err(|e| TransportError::Io(format!("tls: {e}")))?;
        let mut sock = connect(&parsed)?;
        configure_socket(&sock)?;
        let mut tls = Stream::new(&mut client, &mut sock);
        write_http_request(&mut tls, &parsed)?;
        let response = read_entire(&mut tls, MAX_RESPONSE_BYTES)?;
        parse_http_response(response)
    }

    fn post(
        &self,
        url: &str,
        body: &[u8],
        content_type: &str,
        auth_token: Option<&str>,
    ) -> Result<Vec<u8>, TransportError> {
        let parsed = parse_url(url)?;
        if parsed.scheme == "http" {
            if self.http_allowed(&parsed.host) {
                return HttpTransport.post(url, body, content_type, auth_token);
            }
            return Err(TransportError::InsecureScheme(url.to_string()));
        }
        if parsed.scheme != "https" {
            return Err(TransportError::BadUrl(url.to_string()));
        }
        let server_name = ServerName::try_from(parsed.host.clone())
            .map_err(|e| TransportError::BadUrl(format!("{url}: {e}")))?;
        let mut client = ClientConnection::new(Arc::clone(&self.config), server_name)
            .map_err(|e| TransportError::Io(format!("tls: {e}")))?;
        let mut sock = connect(&parsed)?;
        configure_socket(&sock)?;
        let mut tls = Stream::new(&mut client, &mut sock);
        write_http_post(&mut tls, &parsed, body, content_type, auth_token)?;
        let response = read_entire(&mut tls, MAX_RESPONSE_BYTES)?;
        parse_http_response(response)
    }

    fn get_to_writer(&self, url: &str, out: &mut dyn Write) -> Result<(), TransportError> {
        let parsed = parse_url(url)?;
        if parsed.scheme == "http" {
            if self.http_allowed(&parsed.host) {
                return HttpTransport.get_to_writer(url, out);
            }
            return Err(TransportError::InsecureScheme(url.to_string()));
        }
        if parsed.scheme != "https" {
            return Err(TransportError::BadUrl(url.to_string()));
        }
        let server_name = ServerName::try_from(parsed.host.clone())
            .map_err(|e| TransportError::BadUrl(format!("{url}: {e}")))?;
        let mut client = ClientConnection::new(Arc::clone(&self.config), server_name)
            .map_err(|e| TransportError::Io(format!("tls: {e}")))?;
        let mut sock = connect(&parsed)?;
        configure_socket(&sock)?;
        let mut tls = Stream::new(&mut client, &mut sock);
        write_http_request(&mut tls, &parsed)?;
        read_http_response_to_writer(&mut tls, out)
    }

    fn post_reader(
        &self,
        url: &str,
        body: &mut dyn Read,
        body_len: usize,
        content_type: &str,
        auth_token: Option<&str>,
    ) -> Result<Vec<u8>, TransportError> {
        let parsed = parse_url(url)?;
        if parsed.scheme == "http" {
            if self.http_allowed(&parsed.host) {
                return HttpTransport.post_reader(url, body, body_len, content_type, auth_token);
            }
            return Err(TransportError::InsecureScheme(url.to_string()));
        }
        if parsed.scheme != "https" {
            return Err(TransportError::BadUrl(url.to_string()));
        }
        let server_name = ServerName::try_from(parsed.host.clone())
            .map_err(|e| TransportError::BadUrl(format!("{url}: {e}")))?;
        let mut client = ClientConnection::new(Arc::clone(&self.config), server_name)
            .map_err(|e| TransportError::Io(format!("tls: {e}")))?;
        let mut sock = connect(&parsed)?;
        configure_socket(&sock)?;
        let mut tls = Stream::new(&mut client, &mut sock);
        write_http_post_reader(&mut tls, &parsed, body, body_len, content_type, auth_token)?;
        let mut response = Vec::new();
        read_http_response_to_writer(&mut tls, &mut response)?;
        Ok(response)
    }

    fn post_reader_with_headers(
        &self,
        url: &str,
        body: &mut dyn Read,
        body_len: usize,
        content_type: &str,
        auth_token: Option<&str>,
        headers: &[(&str, &str)],
    ) -> Result<Vec<u8>, TransportError> {
        let parsed = parse_url(url)?;
        if parsed.scheme == "http" {
            if self.http_allowed(&parsed.host) {
                return HttpTransport.post_reader_with_headers(
                    url,
                    body,
                    body_len,
                    content_type,
                    auth_token,
                    headers,
                );
            }
            return Err(TransportError::InsecureScheme(url.to_string()));
        }
        if parsed.scheme != "https" {
            return Err(TransportError::BadUrl(url.to_string()));
        }
        let server_name = ServerName::try_from(parsed.host.clone())
            .map_err(|e| TransportError::BadUrl(format!("{url}: {e}")))?;
        let mut client = ClientConnection::new(Arc::clone(&self.config), server_name)
            .map_err(|e| TransportError::Io(format!("tls: {e}")))?;
        let mut sock = connect(&parsed)?;
        configure_socket(&sock)?;
        let mut tls = Stream::new(&mut client, &mut sock);
        write_http_post_reader_with_headers(
            &mut tls,
            &parsed,
            body,
            body_len,
            content_type,
            auth_token,
            headers,
        )?;
        let mut response = Vec::new();
        read_http_response_to_writer(&mut tls, &mut response)?;
        Ok(response)
    }
}

/// In-memory transport keyed by URL. Useful for tests and for the
/// registry's synthetic-catalogue mode.
#[derive(Debug, Default, Clone)]
pub struct StaticTransport {
    entries: HashMap<String, Vec<u8>>,
}

impl StaticTransport {
    /// Constructs an empty transport.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a URL → bytes mapping.
    pub fn insert(&mut self, url: impl Into<String>, bytes: impl Into<Vec<u8>>) {
        self.entries.insert(url.into(), bytes.into());
    }
}

impl Transport for StaticTransport {
    fn get(&self, url: &str) -> Result<Vec<u8>, TransportError> {
        self.entries
            .get(url)
            .cloned()
            .ok_or_else(|| TransportError::BadUrl(format!("static transport missing {url}")))
    }

    fn get_to_writer(&self, url: &str, out: &mut dyn Write) -> Result<(), TransportError> {
        let bytes = self
            .entries
            .get(url)
            .ok_or_else(|| TransportError::BadUrl(format!("static transport missing {url}")))?;
        out.write_all(bytes)
            .map_err(|e| TransportError::Io(format!("write response: {e}")))
    }
}

fn write_http_request<W: Write>(out: &mut W, parsed: &ParsedUrl) -> Result<(), TransportError> {
    let host = parsed.host_header();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: gos-pkg/{version}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        path = parsed.path,
        host = host,
        version = env!("CARGO_PKG_VERSION"),
    );
    out.write_all(request.as_bytes())
        .map_err(|e| TransportError::Io(format!("write: {e}")))
}

fn write_http_post<W: Write>(
    out: &mut W,
    parsed: &ParsedUrl,
    body: &[u8],
    content_type: &str,
    auth_token: Option<&str>,
) -> Result<(), TransportError> {
    if has_header_control(content_type)
        || auth_token.is_some_and(has_header_control)
        || content_type.is_empty()
    {
        return Err(TransportError::BadUrl(
            "unsafe HTTP header value".to_string(),
        ));
    }
    let auth_header = match auth_token {
        Some(token) if !token.is_empty() => format!("Authorization: Bearer {token}\r\n"),
        _ => String::new(),
    };
    let host = parsed.host_header();
    let header = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: gos-pkg/{version}\r\nContent-Type: {ct}\r\nContent-Length: {len}\r\n{auth}Accept: */*\r\nConnection: close\r\n\r\n",
        path = parsed.path,
        host = host,
        version = env!("CARGO_PKG_VERSION"),
        ct = content_type,
        len = body.len(),
        auth = auth_header,
    );
    out.write_all(header.as_bytes())
        .map_err(|e| TransportError::Io(format!("write header: {e}")))?;
    out.write_all(body)
        .map_err(|e| TransportError::Io(format!("write body: {e}")))?;
    out.flush()
        .map_err(|e| TransportError::Io(format!("flush: {e}")))
}

/// Writes a fixed-size request directly from a reader. This is the shared
/// streaming half of the publish path: callers can generate a JSON envelope
/// incrementally instead of first allocating its hex-encoded archive.
fn write_http_post_reader<W: Write>(
    out: &mut W,
    parsed: &ParsedUrl,
    body: &mut dyn Read,
    body_len: usize,
    content_type: &str,
    auth_token: Option<&str>,
) -> Result<(), TransportError> {
    write_http_post_reader_with_headers(out, parsed, body, body_len, content_type, auth_token, &[])
}

fn write_http_post_reader_with_headers<W: Write>(
    out: &mut W,
    parsed: &ParsedUrl,
    body: &mut dyn Read,
    body_len: usize,
    content_type: &str,
    auth_token: Option<&str>,
    extra_headers: &[(&str, &str)],
) -> Result<(), TransportError> {
    if has_header_control(content_type)
        || auth_token.is_some_and(has_header_control)
        || content_type.is_empty()
    {
        return Err(TransportError::BadUrl(
            "unsafe HTTP header value".to_string(),
        ));
    }
    let auth_header = match auth_token {
        Some(token) if !token.is_empty() => format!("Authorization: Bearer {token}\r\n"),
        _ => String::new(),
    };
    let mut protocol_headers = String::new();
    for (name, value) in extra_headers {
        if !is_http_token(name) || has_header_control(value) || value.is_empty() {
            return Err(TransportError::BadUrl(
                "unsafe HTTP protocol header".to_string(),
            ));
        }
        protocol_headers.push_str(name);
        protocol_headers.push_str(": ");
        protocol_headers.push_str(value);
        protocol_headers.push_str("\r\n");
    }
    let host = parsed.host_header();
    let header = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: gos-pkg/{version}\r\nContent-Type: {ct}\r\nContent-Length: {len}\r\n{auth}{protocol_headers}Accept: */*\r\nConnection: close\r\n\r\n",
        path = parsed.path,
        host = host,
        version = env!("CARGO_PKG_VERSION"),
        ct = content_type,
        len = body_len,
        auth = auth_header,
        protocol_headers = protocol_headers,
    );
    out.write_all(header.as_bytes())
        .map_err(|e| TransportError::Io(format!("write header: {e}")))?;
    let mut remaining = body_len;
    let mut buf = [0u8; 8192];
    while remaining != 0 {
        let want = remaining.min(buf.len());
        let n = body
            .read(&mut buf[..want])
            .map_err(|e| TransportError::Io(format!("read request: {e}")))?;
        if n == 0 {
            return Err(TransportError::Io(
                "request body ended before declared Content-Length".to_string(),
            ));
        }
        out.write_all(&buf[..n])
            .map_err(|e| TransportError::Io(format!("write body: {e}")))?;
        remaining -= n;
    }
    out.flush()
        .map_err(|e| TransportError::Io(format!("flush: {e}")))
}

/// Parses an HTTP/1.x response while forwarding its decoded body to `out`.
/// Unlike [`parse_http_response`], this keeps only the header and one 8 KiB
/// transfer buffer resident. It intentionally supports the same conservative
/// protocol subset as the legacy buffered path.
fn read_http_response_to_writer<R: Read>(
    input: &mut R,
    out: &mut dyn Write,
) -> Result<(), TransportError> {
    let mut input = BufReader::with_capacity(8192, input);
    let mut line = Vec::new();
    read_header_line(&mut input, &mut line, 0)?;
    let status_line = std::str::from_utf8(trim_crlf(&line))
        .map_err(|e| TransportError::Io(format!("headers not utf-8: {e}")))?;
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or("");
    let status_text = parts.next().unwrap_or("0");
    let reason = parts.next().unwrap_or("").to_string();
    let status: u16 = status_text.parse().unwrap_or(0);
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1") || !(200..300).contains(&status) {
        return Err(TransportError::BadStatus { status, reason });
    }

    let mut header_bytes = line.len();
    let mut chunked = false;
    let mut content_length = None;
    loop {
        line.clear();
        read_header_line(&mut input, &mut line, header_bytes)?;
        header_bytes = header_bytes.saturating_add(line.len());
        if trim_crlf(&line).is_empty() {
            break;
        }
        let text = std::str::from_utf8(trim_crlf(&line))
            .map_err(|e| TransportError::Io(format!("headers not utf-8: {e}")))?;
        let (name, value) = text
            .split_once(':')
            .ok_or_else(|| TransportError::Io("malformed response header".to_string()))?;
        if !is_http_token(name) || value.bytes().any(|byte| matches!(byte, b'\r' | b'\n')) {
            return Err(TransportError::Io("malformed response header".to_string()));
        }
        let value = value.trim_matches([' ', '\t']);
        if name.eq_ignore_ascii_case("Transfer-Encoding") {
            if chunked || !value.eq_ignore_ascii_case("chunked") {
                return Err(TransportError::Io(
                    "unsupported or repeated Transfer-Encoding".to_string(),
                ));
            }
            chunked = true;
        } else if name.eq_ignore_ascii_case("Content-Length") {
            if content_length.is_some()
                || value.is_empty()
                || !value.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(TransportError::Io(
                    "invalid or repeated Content-Length".to_string(),
                ));
            }
            content_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| TransportError::Io("invalid Content-Length".to_string()))?,
            );
        }
    }
    if chunked && content_length.is_some() {
        return Err(TransportError::Io(
            "response has both Transfer-Encoding and Content-Length".to_string(),
        ));
    }
    if chunked {
        return copy_chunked_to_writer(&mut input, out);
    }
    if let Some(length) = content_length {
        if length > MAX_RESPONSE_BYTES {
            return Err(TransportError::ResponseTooLarge {
                limit: MAX_RESPONSE_BYTES,
            });
        }
        return copy_exact_to_writer(&mut input, out, length);
    }
    copy_until_eof_to_writer(&mut input, out)
}

fn read_header_line<R: BufRead>(
    input: &mut R,
    line: &mut Vec<u8>,
    prior_bytes: usize,
) -> Result<(), TransportError> {
    read_crlf_line_limited(input, line, MAX_RESPONSE_HEADER_BYTES, "header")?;
    if line.is_empty()
        || !line.ends_with(b"\r\n")
        || prior_bytes.saturating_add(line.len()) > MAX_RESPONSE_HEADER_BYTES
    {
        return Err(TransportError::Io(
            "response headers exceed limit or are malformed".to_string(),
        ));
    }
    Ok(())
}

/// Reads one CRLF-delimited protocol line without letting an attacker force
/// `read_until` to grow a vector without bound before a later size check.
fn read_crlf_line_limited<R: BufRead>(
    input: &mut R,
    line: &mut Vec<u8>,
    limit: usize,
    kind: &str,
) -> Result<(), TransportError> {
    loop {
        let (consumed, finished) = {
            let available = input
                .fill_buf()
                .map_err(|e| TransportError::Io(format!("read {kind}: {e}")))?;
            if available.is_empty() {
                return Err(TransportError::Io(format!("truncated {kind}")));
            }
            let take = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            if line.len().saturating_add(take) > limit {
                return Err(TransportError::Io(format!("{kind} exceeds limit")));
            }
            line.extend_from_slice(&available[..take]);
            (take, take < available.len() || available[take - 1] == b'\n')
        };
        input.consume(consumed);
        if finished {
            return Ok(());
        }
    }
}

fn trim_crlf(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r\n").unwrap_or(line)
}

fn copy_exact_to_writer<R: Read>(
    input: &mut R,
    out: &mut dyn Write,
    mut remaining: usize,
) -> Result<(), TransportError> {
    let mut buf = [0u8; 8192];
    while remaining != 0 {
        let take = remaining.min(buf.len());
        let n = input
            .read(&mut buf[..take])
            .map_err(|e| TransportError::Io(format!("read body: {e}")))?;
        if n == 0 {
            return Err(TransportError::Io("truncated HTTP body".to_string()));
        }
        out.write_all(&buf[..n])
            .map_err(|e| TransportError::Io(format!("write response: {e}")))?;
        remaining -= n;
    }
    Ok(())
}

fn copy_until_eof_to_writer<R: Read>(
    input: &mut R,
    out: &mut dyn Write,
) -> Result<(), TransportError> {
    let mut total = 0usize;
    let mut buf = [0u8; 8192];
    loop {
        match input.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                total = total
                    .checked_add(n)
                    .ok_or(TransportError::ResponseTooLarge {
                        limit: MAX_RESPONSE_BYTES,
                    })?;
                if total > MAX_RESPONSE_BYTES {
                    return Err(TransportError::ResponseTooLarge {
                        limit: MAX_RESPONSE_BYTES,
                    });
                }
                out.write_all(&buf[..n])
                    .map_err(|e| TransportError::Io(format!("write response: {e}")))?;
            }
            Err(e)
                if total != 0
                    && matches!(
                        e.kind(),
                        std::io::ErrorKind::UnexpectedEof
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::ConnectionReset
                    ) =>
            {
                return Ok(());
            }
            Err(e) => return Err(TransportError::Io(format!("read body: {e}"))),
        }
    }
}

fn copy_chunked_to_writer<R: BufRead>(
    input: &mut R,
    out: &mut dyn Write,
) -> Result<(), TransportError> {
    let mut total = 0usize;
    let mut trailer_bytes = 0usize;
    let mut line = Vec::new();
    loop {
        line.clear();
        read_crlf_line_limited(input, &mut line, MAX_RESPONSE_HEADER_BYTES, "chunk size")?;
        if line.is_empty() || !line.ends_with(b"\r\n") {
            return Err(TransportError::Io("truncated chunk size".to_string()));
        }
        let size_line = std::str::from_utf8(trim_crlf(&line))
            .map_err(|e| TransportError::Io(format!("chunk size: {e}")))?;
        let size_text = size_line.split(';').next().unwrap_or(size_line).trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|e| TransportError::Io(format!("chunk hex size `{size_text}`: {e}")))?;
        if size == 0 {
            loop {
                line.clear();
                read_crlf_line_limited(
                    input,
                    &mut line,
                    MAX_RESPONSE_HEADER_BYTES,
                    "chunk trailer",
                )?;
                trailer_bytes = trailer_bytes
                    .checked_add(line.len())
                    .ok_or_else(|| TransportError::Io("chunk trailers exceed limit".to_string()))?;
                if trailer_bytes > MAX_RESPONSE_HEADER_BYTES {
                    return Err(TransportError::Io(
                        "chunk trailers exceed limit".to_string(),
                    ));
                }
                if line.is_empty() || !line.ends_with(b"\r\n") {
                    return Err(TransportError::Io("truncated chunk trailer".to_string()));
                }
                if trim_crlf(&line).is_empty() {
                    return Ok(());
                }
            }
        }
        total = total
            .checked_add(size)
            .ok_or(TransportError::ResponseTooLarge {
                limit: MAX_RESPONSE_BYTES,
            })?;
        if total > MAX_RESPONSE_BYTES {
            return Err(TransportError::ResponseTooLarge {
                limit: MAX_RESPONSE_BYTES,
            });
        }
        copy_exact_to_writer(input, out, size)?;
        let mut crlf = [0u8; 2];
        input
            .read_exact(&mut crlf)
            .map_err(|e| TransportError::Io(format!("read chunk terminator: {e}")))?;
        if crlf != *b"\r\n" {
            return Err(TransportError::Io("chunk overruns body".to_string()));
        }
    }
}

fn read_entire<R: Read>(r: &mut R, limit: usize) -> Result<Vec<u8>, TransportError> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if out.len().saturating_add(n) > limit {
                    return Err(TransportError::ResponseTooLarge { limit });
                }
                out.extend_from_slice(&buf[..n]);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(TransportError::Io("read would block".to_string()));
            }
            Err(e) => {
                // TLS close-notify may surface as UnexpectedEof on
                // some servers; treat any bytes we already read as
                // the body and return success. `ConnectionReset` is
                // also common when a server writes the response and
                // immediately closes the socket without a graceful
                // close-notify (most one-shot test servers and a
                // few production proxies behave this way).
                if !out.is_empty()
                    && matches!(
                        e.kind(),
                        std::io::ErrorKind::UnexpectedEof
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::ConnectionReset
                    )
                {
                    break;
                }
                return Err(TransportError::Io(format!("read: {e}")));
            }
        }
    }
    Ok(out)
}

/// Parses an owned HTTP response and retains its allocation for an
/// unchunked body. Keeping this owned avoids the old `body.to_vec()` peak
/// where a 64 MiB response transiently occupied two full body allocations.
fn parse_http_response(mut response: Vec<u8>) -> Result<Vec<u8>, TransportError> {
    let (header_len, body_start) = split_head_body_offset(&response)
        .ok_or_else(|| TransportError::Io("response missing CRLFCRLF".to_string()))?;
    if header_len > MAX_RESPONSE_HEADER_BYTES {
        return Err(TransportError::Io(
            "response headers exceed limit".to_string(),
        ));
    }
    let header_text = std::str::from_utf8(&response[..header_len])
        .map_err(|e| TransportError::Io(format!("headers not utf-8: {e}")))?;
    let mut lines = header_text.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or("");
    let status_text = parts.next().unwrap_or("0");
    let reason = parts.next().unwrap_or("").to_string();
    let status: u16 = status_text.parse().unwrap_or(0);
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1") || !(200..300).contains(&status) {
        return Err(TransportError::BadStatus { status, reason });
    }
    let mut chunked = false;
    let mut content_length = None;
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| TransportError::Io("malformed response header".to_string()))?;
        if !is_http_token(name) || value.bytes().any(|byte| matches!(byte, b'\r' | b'\n')) {
            return Err(TransportError::Io("malformed response header".to_string()));
        }
        let value = value.trim_matches([' ', '\t']);
        if name.eq_ignore_ascii_case("Transfer-Encoding") {
            if chunked || !value.eq_ignore_ascii_case("chunked") {
                return Err(TransportError::Io(
                    "unsupported or repeated Transfer-Encoding".to_string(),
                ));
            }
            chunked = true;
        } else if name.eq_ignore_ascii_case("Content-Length") {
            if content_length.is_some()
                || value.is_empty()
                || !value.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(TransportError::Io(
                    "invalid or repeated Content-Length".to_string(),
                ));
            }
            content_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| TransportError::Io("invalid Content-Length".to_string()))?,
            );
        }
    }
    if chunked && content_length.is_some() {
        return Err(TransportError::Io(
            "response has both Transfer-Encoding and Content-Length".to_string(),
        ));
    }
    if chunked {
        decode_chunked_in_place(&mut response, body_start, MAX_RESPONSE_BYTES)?;
        response.drain(..body_start);
        return Ok(response);
    }
    let body_len = response.len().saturating_sub(body_start);
    if content_length.is_some_and(|expected| expected != body_len) {
        return Err(TransportError::Io(
            "truncated or overlong HTTP body".to_string(),
        ));
    }
    response.drain(..body_start);
    Ok(response)
}

fn is_http_token(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn split_head_body_offset(bytes: &[u8]) -> Option<(usize, usize)> {
    let sep = b"\r\n\r\n";
    for (i, window) in bytes.windows(sep.len()).enumerate() {
        if window == sep {
            return Some((i, i + sep.len()));
        }
    }
    None
}

/// Decodes a chunked body directly inside `response`. `write` is always at
/// or behind `cursor`, so `copy_within` never needs another response-sized
/// allocation. The caller removes the headers after decoding.
fn decode_chunked_in_place(
    response: &mut Vec<u8>,
    body_start: usize,
    limit: usize,
) -> Result<(), TransportError> {
    let mut cursor = body_start;
    let mut write = body_start;
    while cursor < response.len() {
        let Some(crlf) = find_crlf(&response[cursor..]) else {
            return Err(TransportError::Io("truncated chunk size".to_string()));
        };
        let size_line = std::str::from_utf8(&response[cursor..cursor + crlf])
            .map_err(|e| TransportError::Io(format!("chunk size: {e}")))?;
        let size_text = size_line.split(';').next().unwrap_or(size_line).trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|e| TransportError::Io(format!("chunk hex size `{size_text}`: {e}")))?;
        cursor += crlf + 2;
        if size == 0 {
            if response[cursor..].starts_with(b"\r\n") {
                response.truncate(write);
                return Ok(());
            }
            while cursor < response.len() {
                let Some(trailer_end) = find_crlf(&response[cursor..]) else {
                    return Err(TransportError::Io("truncated chunk trailer".to_string()));
                };
                cursor += trailer_end + 2;
                if trailer_end == 0 {
                    response.truncate(write);
                    return Ok(());
                }
            }
            return Err(TransportError::Io(
                "missing final chunk trailer".to_string(),
            ));
        }
        let end = cursor
            .checked_add(size)
            .and_then(|end| end.checked_add(2))
            .ok_or_else(|| TransportError::Io("chunk size overflow".to_string()))?;
        if end > response.len() || &response[cursor + size..end] != b"\r\n" {
            return Err(TransportError::Io("chunk overruns body".to_string()));
        }
        let next_write = write
            .checked_add(size)
            .ok_or(TransportError::ResponseTooLarge { limit })?;
        if next_write - body_start > limit {
            return Err(TransportError::ResponseTooLarge { limit });
        }
        response.copy_within(cursor..cursor + size, write);
        write = next_write;
        cursor = end;
    }
    Err(TransportError::Io("missing final chunk".to_string()))
}

fn find_crlf(bytes: &[u8]) -> Option<usize> {
    bytes.windows(2).position(|window| window == b"\r\n")
}

/// Fetches `url` through `transport` and returns the body only if its
/// SHA-256 hex digest matches `expected_sha256`. Mismatches return
/// [`TransportError::DigestMismatch`] and the body is dropped.
pub fn fetch_verified(
    transport: &dyn Transport,
    url: &str,
    expected_sha256: &str,
) -> Result<Vec<u8>, TransportError> {
    let mut body = Vec::new();
    fetch_verified_to_writer(transport, url, expected_sha256, &mut body)?;
    Ok(body)
}

/// Streams `url` into `out` and verifies the streamed bytes' SHA-256 digest.
/// This is the migration target for callers that do not need a `Vec<u8>` copy.
pub fn fetch_verified_to_writer(
    transport: &dyn Transport,
    url: &str,
    expected_sha256: &str,
    out: &mut dyn Write,
) -> Result<(), TransportError> {
    let mut writer = HashingWriter::new(out);
    transport.get_to_writer(url, &mut writer)?;
    let actual = writer.finish();
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err(TransportError::DigestMismatch {
            expected: expected_sha256.to_string(),
            actual,
        });
    }
    Ok(())
}

struct HashingWriter<'a> {
    out: &'a mut dyn Write,
    hasher: sha256::Hasher,
}

impl<'a> HashingWriter<'a> {
    fn new(out: &'a mut dyn Write) -> Self {
        Self {
            out,
            hasher: sha256::Hasher::new(),
        }
    }

    fn finish(self) -> String {
        self.hasher.finalize_hex()
    }
}

impl Write for HashingWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.out.write_all(bytes)?;
        self.hasher.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_url_extracts_parts() {
        let parsed = parse_url("https://example.com/packages/a.tgz").unwrap();
        assert_eq!(parsed.scheme, "https");
        assert_eq!(parsed.host, "example.com");
        assert_eq!(parsed.port, 443);
        assert_eq!(parsed.path, "/packages/a.tgz");
    }

    #[test]
    fn parse_url_honours_explicit_port() {
        let parsed = parse_url("http://localhost:8080/index").unwrap();
        assert_eq!(parsed.port, 8080);
    }

    #[test]
    fn parse_url_accepts_bracketed_ipv6_and_rejects_header_injection() {
        let parsed = parse_url("http://[::1]:8080/index").unwrap();
        assert_eq!(parsed.host, "::1");
        assert_eq!(parsed.port, 8080);
        assert!(parse_url("https://example.test/ok\r\nInjected: yes").is_err());
        assert!(parse_url("https://user@example.test/index").is_err());
    }

    #[test]
    fn parse_url_preserves_query_but_rejects_fragments_and_ambiguous_authorities() {
        let parsed = parse_url("https://example.test/package?version=1%2E2").unwrap();
        assert_eq!(parsed.path, "/package?version=1%2E2");
        assert!(parse_url("https://example.test/package#fragment").is_err());
        assert!(parse_url("https://example.test\\@attacker.test/package").is_err());
        assert!(parse_url("https://example.test:99999/package").is_err());
    }

    #[test]
    fn chunked_decoder_rejects_truncated_and_bad_terminators() {
        let header = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert!(parse_http_response([header.as_slice(), b"3\r\nabc"].concat()).is_err());
        assert!(parse_http_response([header.as_slice(), b"3\r\nabcXX0\r\n\r\n"].concat()).is_err());
        assert_eq!(
            parse_http_response([header.as_slice(), b"3\r\nabc\r\n0\r\n\r\n"].concat()).unwrap(),
            b"abc"
        );
    }

    #[test]
    fn response_parser_rejects_ambiguous_or_unsupported_framing() {
        assert!(parse_http_response(
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n".to_vec()
        )
        .is_err());
        assert!(
            parse_http_response(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n\r\n".to_vec()
            )
            .is_err()
        );
        assert!(
            parse_http_response(
                b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\na".to_vec()
            )
            .is_err()
        );
    }

    #[test]
    fn response_parser_rejects_overlarge_and_malformed_headers() {
        let mut response = b"HTTP/1.1 200 OK\r\nX-Test: ".to_vec();
        response.resize(MAX_RESPONSE_HEADER_BYTES + 1, b'a');
        response.extend_from_slice(b"\r\n\r\n");
        assert!(parse_http_response(response).is_err());
        assert!(parse_http_response(b"HTTP/1.1 200 OK\r\nNot A Header\r\n\r\n".to_vec()).is_err());
    }

    #[test]
    fn response_parser_reuses_body_buffer_for_content_length_and_chunked() {
        assert_eq!(
            parse_http_response(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".to_vec())
                .unwrap(),
            b"hello"
        );
        assert_eq!(
            parse_http_response(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhe\r\n3\r\nllo\r\n0\r\n\r\n".to_vec()
            )
            .unwrap(),
            b"hello"
        );
    }

    #[test]
    fn static_transport_serves_registered_urls() {
        let mut t = StaticTransport::new();
        t.insert("https://example.test/foo", b"hello".to_vec());
        let body = t.get("https://example.test/foo").unwrap();
        assert_eq!(body, b"hello");
    }

    #[test]
    fn fetch_verified_detects_digest_mismatch() {
        let mut t = StaticTransport::new();
        t.insert("https://example.test/foo", b"hello".to_vec());
        let good = sha256::hex(b"hello");
        assert_eq!(
            fetch_verified(&t, "https://example.test/foo", &good).unwrap(),
            b"hello"
        );
        let err =
            fetch_verified(&t, "https://example.test/foo", "00".repeat(32).as_str()).unwrap_err();
        assert!(matches!(err, TransportError::DigestMismatch { .. }));
    }

    #[test]
    fn streaming_response_decoder_forwards_chunked_body_without_buffering_response() {
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n";
        let mut input = std::io::Cursor::new(response);
        let mut out = Vec::new();
        read_http_response_to_writer(&mut input, &mut out).unwrap();
        assert_eq!(out, b"abcde");
    }

    #[test]
    fn http_transport_rejects_https_url() {
        let err = HttpTransport.get("https://example.com").unwrap_err();
        assert!(matches!(err, TransportError::HttpsUnsupported));
    }

    #[test]
    fn is_loopback_host_recognizes_local_addresses() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("LocalHost"));
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("127.5.6.7"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("registry.evil.test"));
        assert!(!is_loopback_host("10.0.0.1"));
    }

    #[test]
    fn https_transport_refuses_remote_http_by_default() {
        // The scheme is rejected before any network connection.
        let err = HttpsTransport::new_mozilla_roots()
            .get("http://registry.evil.test/index.json")
            .unwrap_err();
        assert!(
            matches!(err, TransportError::InsecureScheme(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn https_transport_http_policy_respects_loopback_and_opt_in() {
        let secure = HttpsTransport::new_mozilla_roots();
        assert!(!secure.http_allowed("registry.evil.test"));
        assert!(secure.http_allowed("localhost"));
        assert!(secure.http_allowed("127.0.0.1"));

        let insecure = HttpsTransport::new_mozilla_roots_insecure();
        assert!(insecure.http_allowed("registry.evil.test"));
    }
}
