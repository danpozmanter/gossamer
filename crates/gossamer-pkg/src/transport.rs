//! HTTP(S) transport for the package fetcher.
//! Two real implementations plus a test double:
//! - [`HttpTransport`] — plain `http://` over [`std::net::TcpStream`].
//! - [`HttpsTransport`] — `https://` over a `rustls` client session
//!   pinned to the Mozilla-maintained root CAs from `webpki-roots`.
//! - [`StaticTransport`] — in-memory URL → bytes map. Used by tests
//!   and by the registry resolver's synthetic path.
//!
//! Downloaded bytes are always paired with a SHA-256 digest that the
//! fetcher compares against the expected `sha256 = ...` field from
//! the project manifest before admitting the payload into the cache.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use rustls::client::ClientConnection;
use rustls::{ClientConfig, RootCertStore, Stream};
use rustls_pki_types::ServerName;

use crate::sha256;

/// Error shape for transport failures.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum TransportError {
    /// URL did not parse into a scheme+host+path triple we handle.
    #[error("bad url: {0}")]
    BadUrl(String),
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
}

/// Abstract transport the fetcher drives.
pub trait Transport: Send + Sync {
    /// Fetches the body at `url`. Returns the raw bytes, without
    /// interpreting Content-Type.
    fn get(&self, url: &str) -> Result<Vec<u8>, TransportError>;
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
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| TransportError::BadUrl(url.to_string()))?;
    let (authority, path) = rest.split_once('/').map_or_else(
        || (rest.to_string(), "/".to_string()),
        |(a, p)| (a.to_string(), format!("/{p}")),
    );
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>()
                .map_err(|_| TransportError::BadUrl(url.to_string()))?,
        ),
        None => (
            authority,
            if scheme == "https" { 443 } else { 80 },
        ),
    };
    Ok(ParsedUrl {
        scheme: scheme.to_string(),
        host,
        port,
        path,
    })
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
        let address = format!("{}:{}", parsed.host, parsed.port);
        let mut stream = TcpStream::connect(&address)
            .map_err(|e| TransportError::Io(format!("connect {address}: {e}")))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .map_err(|e| TransportError::Io(format!("read_timeout: {e}")))?;
        write_http_request(&mut stream, &parsed)?;
        let response = read_entire(&mut stream)?;
        parse_http_response(&response)
    }
}

/// Rustls-backed HTTPS transport, pinned to the Mozilla CA bundle.
pub struct HttpsTransport {
    config: Arc<ClientConfig>,
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
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Self {
            config: Arc::new(config),
        }
    }
}

impl Transport for HttpsTransport {
    fn get(&self, url: &str) -> Result<Vec<u8>, TransportError> {
        let parsed = parse_url(url)?;
        if parsed.scheme == "http" {
            return HttpTransport.get(url);
        }
        if parsed.scheme != "https" {
            return Err(TransportError::BadUrl(url.to_string()));
        }
        let server_name = ServerName::try_from(parsed.host.clone())
            .map_err(|e| TransportError::BadUrl(format!("{url}: {e}")))?;
        let mut client = ClientConnection::new(Arc::clone(&self.config), server_name)
            .map_err(|e| TransportError::Io(format!("tls: {e}")))?;
        let address = format!("{}:{}", parsed.host, parsed.port);
        let mut sock = TcpStream::connect(&address)
            .map_err(|e| TransportError::Io(format!("connect {address}: {e}")))?;
        sock.set_read_timeout(Some(Duration::from_secs(30)))
            .map_err(|e| TransportError::Io(format!("read_timeout: {e}")))?;
        let mut tls = Stream::new(&mut client, &mut sock);
        write_http_request(&mut tls, &parsed)?;
        let response = read_entire(&mut tls)?;
        parse_http_response(&response)
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
}

fn write_http_request<W: Write>(out: &mut W, parsed: &ParsedUrl) -> Result<(), TransportError> {
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: gos-pkg/{version}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        path = parsed.path,
        host = parsed.host,
        version = env!("CARGO_PKG_VERSION"),
    );
    out.write_all(request.as_bytes())
        .map_err(|e| TransportError::Io(format!("write: {e}")))
}

fn read_entire<R: Read>(r: &mut R) -> Result<Vec<u8>, TransportError> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => {
                // TLS close-notify may surface as UnexpectedEof on
                // some servers; treat any bytes we already read as
                // the body and return success.
                if !out.is_empty()
                    && (e.kind() == std::io::ErrorKind::UnexpectedEof
                        || e.kind() == std::io::ErrorKind::ConnectionAborted)
                {
                    break;
                }
                return Err(TransportError::Io(format!("read: {e}")));
            }
        }
    }
    Ok(out)
}

fn parse_http_response(response: &[u8]) -> Result<Vec<u8>, TransportError> {
    let (headers, body) = split_head_body(response)
        .ok_or_else(|| TransportError::Io("response missing CRLFCRLF".to_string()))?;
    let header_text = std::str::from_utf8(headers)
        .map_err(|e| TransportError::Io(format!("headers not utf-8: {e}")))?;
    let status_line = header_text.lines().next().unwrap_or("");
    let mut parts = status_line.splitn(3, ' ');
    let _version = parts.next().unwrap_or("");
    let status_text = parts.next().unwrap_or("0");
    let reason = parts.next().unwrap_or("").to_string();
    let status: u16 = status_text.parse().unwrap_or(0);
    if !(200..300).contains(&status) {
        return Err(TransportError::BadStatus { status, reason });
    }
    let mut chunked = false;
    for line in header_text.lines().skip(1) {
        if line.eq_ignore_ascii_case("Transfer-Encoding: chunked") {
            chunked = true;
        }
    }
    if chunked {
        return decode_chunked(body);
    }
    Ok(body.to_vec())
}

fn split_head_body(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let sep = b"\r\n\r\n";
    for (i, window) in bytes.windows(sep.len()).enumerate() {
        if window == sep {
            return Some((&bytes[..i], &bytes[i + sep.len()..]));
        }
    }
    None
}

fn decode_chunked(body: &[u8]) -> Result<Vec<u8>, TransportError> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while cursor < body.len() {
        let Some(crlf) = find_crlf(&body[cursor..]) else {
            break;
        };
        let size_line = std::str::from_utf8(&body[cursor..cursor + crlf])
            .map_err(|e| TransportError::Io(format!("chunk size: {e}")))?;
        let size_text = size_line.split(';').next().unwrap_or(size_line).trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|e| TransportError::Io(format!("chunk hex size `{size_text}`: {e}")))?;
        cursor += crlf + 2;
        if size == 0 {
            break;
        }
        if cursor + size > body.len() {
            return Err(TransportError::Io("chunk overruns body".to_string()));
        }
        out.extend_from_slice(&body[cursor..cursor + size]);
        cursor += size + 2;
    }
    Ok(out)
}

fn find_crlf(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(2)
        .position(|window| window == b"\r\n")
}

/// Fetches `url` through `transport` and returns the body only if its
/// SHA-256 hex digest matches `expected_sha256`. Mismatches return
/// [`TransportError::DigestMismatch`] and the body is dropped.
pub fn fetch_verified(
    transport: &dyn Transport,
    url: &str,
    expected_sha256: &str,
) -> Result<Vec<u8>, TransportError> {
    let body = transport.get(url)?;
    let actual = sha256::hex(&body);
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err(TransportError::DigestMismatch {
            expected: expected_sha256.to_string(),
            actual,
        });
    }
    Ok(body)
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
        let err = fetch_verified(&t, "https://example.test/foo", "00".repeat(32).as_str())
            .unwrap_err();
        assert!(matches!(err, TransportError::DigestMismatch { .. }));
    }

    #[test]
    fn http_transport_rejects_https_url() {
        let err = HttpTransport.get("https://example.com").unwrap_err();
        assert!(matches!(err, TransportError::HttpsUnsupported));
    }
}
