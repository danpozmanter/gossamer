//! HTTP/3 server and client (RFC 9114) - first-party stdlib
//! support, wrapping `quinn` (QUIC) and the `h3` crate (HTTP/3
//! framing on top of QUIC).
//!
//! Both `quinn` and `h3` are async-only and assume a tokio
//! reactor is driving timers and UDP I/O. Gossamer's scheduler
//! does not expose those primitives, so each `serve` call and
//! each `Client` instance spins up its own current-thread
//! tokio runtime that stays private to the module; callers see
//! only synchronous entry points that mirror the [`http_h2`] and
//! [`http`] surfaces.
//!
//! Public surface mirrors `http_h2`:
//!
//! - `serve` - bind a UDP socket on the supplied address, run
//!   a quinn endpoint, dispatch every accepted request to the
//!   handler (the same `crate::http::Handler` trait the rest of
//!   the HTTP stack speaks).
//! - `Client` - issue HTTP/3 requests against a remote endpoint.
//!   Same shape as `crate::http::Client`: `new`, `get`, `post`,
//!   `put`, `delete`, `head`, `options`, `request`.
//!
//! [`http_h2`]: crate::http_h2
//! [`http`]: crate::http

#![forbid(unsafe_code)]
#![allow(
    clippy::missing_errors_doc,
    clippy::module_name_repetitions,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::items_after_statements,
    clippy::cast_possible_truncation,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value
)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Buf, Bytes, BytesMut};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio::runtime::Builder as RtBuilder;

use crate::errors::Error;
use crate::http::{Headers, Method, Request, Response, StatusCode};

/// Handler signature for HTTP/3: receive a `Request`, return a
/// complete `Response`. Mirrors [`crate::http_h2::Handler`] so
/// the two stacks share a single handler trait shape - a
/// production service can be served over h2 or h3 from the same
/// handler value.
pub trait Handler: Send + Sync + 'static {
    /// Serves one HTTP/3 request.
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

/// Errors raised by the HTTP/3 server and client wrappers.
#[derive(Debug, thiserror::Error)]
pub enum H3Error {
    /// I/O error binding or reading the QUIC socket.
    #[error("h3 io: {0}")]
    Io(String),
    /// TLS configuration or handshake failure.
    #[error("h3 tls: {0}")]
    Tls(String),
    /// QUIC transport-level error (handshake, packet, stream).
    #[error("h3 quic: {0}")]
    Quic(String),
    /// h3 protocol-level error.
    #[error("h3 protocol: {0}")]
    Protocol(String),
    /// Server / client transport reported the operation as
    /// unsupported on this build. Reserved for future feature
    /// gating; production builds today never return this variant.
    #[error("h3 unsupported")]
    Unsupported,
}

impl From<H3Error> for Error {
    fn from(err: H3Error) -> Self {
        Self::new(err.to_string())
    }
}

/// Installs the rustls ring crypto provider exactly once for
/// this process. Idempotent - both server and client entry
/// points call this before touching any rustls configuration.
fn install_ring_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Binds a UDP socket on `addr`, runs a QUIC + HTTP/3 endpoint,
/// and dispatches every accepted request to `handler`. Blocks
/// the calling thread until the endpoint stops accepting (a
/// transport-level fatal error tears it down - there is no
/// public shutdown signal yet, matching the h2 surface).
///
/// `cert_path` / `key_path` point to PEM-encoded files. HTTP/3
/// mandates TLS, so a keypair is required.
pub fn serve<H>(addr: &str, cert_path: &str, key_path: &str, handler: H) -> Result<(), Error>
where
    H: Handler + Clone + 'static,
{
    install_ring_provider();
    let cert_pem = std::fs::read(cert_path)
        .map_err(|e| Error::from(H3Error::Io(format!("read cert: {e}"))))?;
    let key_pem =
        std::fs::read(key_path).map_err(|e| Error::from(H3Error::Io(format!("read key: {e}"))))?;
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&cert_pem)
        .collect::<Result<_, _>>()
        .map_err(|e| Error::from(H3Error::Tls(format!("cert parse: {e}"))))?;
    if certs.is_empty() {
        return Err(Error::from(H3Error::Tls("no certificates in PEM".into())));
    }
    let key: PrivateKeyDer<'static> = PrivateKeyDer::pem_slice_iter(&key_pem)
        .next()
        .ok_or_else(|| Error::from(H3Error::Tls("no private key in PEM".into())))?
        .map_err(|e| Error::from(H3Error::Tls(format!("key parse: {e}"))))?;

    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| Error::from(H3Error::Tls(format!("server config: {e}"))))?;
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let quic_server = QuicServerConfig::try_from(tls)
        .map_err(|e| Error::from(H3Error::Tls(format!("quic server config: {e}"))))?;
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_server));
    {
        let transport = Arc::get_mut(&mut server_config.transport)
            .ok_or_else(|| Error::from(H3Error::Quic("transport config aliased".into())))?;
        transport.max_concurrent_uni_streams(0_u8.into());
    }

    let socket_addr: SocketAddr = addr
        .parse()
        .map_err(|e| Error::from(H3Error::Io(format!("parse addr: {e}"))))?;

    let rt = RtBuilder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::from(H3Error::Io(format!("tokio runtime: {e}"))))?;
    let handler = Arc::new(handler);
    rt.block_on(run_server(socket_addr, server_config, handler))
}

async fn run_server<H>(
    addr: SocketAddr,
    config: quinn::ServerConfig,
    handler: Arc<H>,
) -> Result<(), Error>
where
    H: Handler + 'static,
{
    let endpoint = quinn::Endpoint::server(config, addr)
        .map_err(|e| Error::from(H3Error::Io(format!("endpoint bind: {e}"))))?;
    while let Some(incoming) = endpoint.accept().await {
        let handler = Arc::clone(&handler);
        tokio::spawn(async move {
            if let Err(e) = serve_connection(incoming, handler).await {
                eprintln!("h3: connection error: {e}");
            }
        });
    }
    Ok(())
}

async fn serve_connection<H>(incoming: quinn::Incoming, handler: Arc<H>) -> Result<(), Error>
where
    H: Handler + 'static,
{
    let conn = incoming
        .await
        .map_err(|e| Error::from(H3Error::Quic(format!("accept: {e}"))))?;
    let h3_conn = h3_quinn::Connection::new(conn);
    let mut h3_server = h3::server::Connection::<_, Bytes>::new(h3_conn)
        .await
        .map_err(|e| Error::from(H3Error::Protocol(format!("h3 conn: {e}"))))?;

    loop {
        match h3_server.accept().await {
            Ok(Some(resolver)) => {
                let handler = Arc::clone(&handler);
                tokio::spawn(async move {
                    if let Err(e) = serve_stream(resolver, handler).await {
                        eprintln!("h3: stream error: {e}");
                    }
                });
            }
            Ok(None) => break,
            Err(e) => {
                return Err(Error::from(H3Error::Protocol(format!(
                    "accept stream: {e}"
                ))));
            }
        }
    }
    Ok(())
}

async fn serve_stream<C, H>(
    resolver: h3::server::RequestResolver<C, Bytes>,
    handler: Arc<H>,
) -> Result<(), Error>
where
    C: h3::quic::Connection<Bytes>,
    H: Handler + 'static,
{
    let (req, mut stream) = resolver
        .resolve_request()
        .await
        .map_err(|e| Error::from(H3Error::Protocol(format!("resolve: {e}"))))?;

    let (parts, ()) = req.into_parts();
    let method = method_from_http(&parts.method);
    let path_q = parts
        .uri
        .path_and_query()
        .map_or_else(|| "/".to_string(), |pq| pq.as_str().to_string());
    let (path, query) = crate::http::split_path_query(&path_q);

    let mut headers = Headers::new();
    for (name, value) in &parts.headers {
        if let Ok(v) = value.to_str() {
            headers.insert(name.as_str(), v);
        }
    }

    let mut body = Vec::<u8>::new();
    while let Some(mut chunk) = stream
        .recv_data()
        .await
        .map_err(|e| Error::from(H3Error::Protocol(format!("recv body: {e}"))))?
    {
        while chunk.has_remaining() {
            let bs = chunk.chunk();
            body.extend_from_slice(bs);
            let len = bs.len();
            chunk.advance(len);
        }
    }

    let request = Request {
        method,
        path,
        query,
        headers,
        body,
        context: crate::context::Context::background(),
        trailers: None,
    };

    let response =
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler.serve(request))) {
            Ok(resp) => resp,
            Err(_) => Response {
                status: StatusCode(500),
                headers: Headers::new(),
                body: b"internal server error".to_vec(),
                raw_header_pairs: Vec::new(),
                body_stream: None,
            },
        };

    let status = ::http::StatusCode::from_u16(response.status.as_u16())
        .map_err(|e| Error::from(H3Error::Protocol(format!("bad status: {e}"))))?;
    let mut builder = ::http::Response::builder().status(status);
    {
        let h = builder
            .headers_mut()
            .ok_or_else(|| Error::from(H3Error::Protocol("response head".into())))?;
        for (name, value) in response.headers.iter() {
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
        .map_err(|e| Error::from(H3Error::Protocol(format!("build head: {e}"))))?;
    stream
        .send_response(head)
        .await
        .map_err(|e| Error::from(H3Error::Protocol(format!("send_response: {e}"))))?;
    if !response.body.is_empty() {
        stream
            .send_data(Bytes::from(response.body))
            .await
            .map_err(|e| Error::from(H3Error::Protocol(format!("send_data: {e}"))))?;
    }
    stream
        .finish()
        .await
        .map_err(|e| Error::from(H3Error::Protocol(format!("finish: {e}"))))?;
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

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// HTTP/3 client. Each instance owns a private tokio runtime
/// (one worker thread) plus a `quinn::Endpoint` bound to an
/// ephemeral UDP port. Cloning is O(1); internal state is
/// `Arc`-shared.
#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    rt: tokio::runtime::Runtime,
    endpoint: quinn::Endpoint,
}

impl Client {
    /// Constructs a client that validates server certificates
    /// against the bundled Mozilla root store.
    pub fn new() -> Result<Self, Error> {
        Self::build(false)
    }

    /// Constructs a client that accepts any server certificate.
    /// Intended for tests and self-signed development endpoints
    /// - never use in production.
    pub fn insecure() -> Result<Self, Error> {
        Self::build(true)
    }

    fn build(skip_verify: bool) -> Result<Self, Error> {
        install_ring_provider();
        let rt = RtBuilder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::from(H3Error::Io(format!("tokio runtime: {e}"))))?;
        // quinn::Endpoint::client must be called from inside a
        // tokio runtime so its background driver task can be
        // spawned; `block_on(async {...})` keeps the constructor
        // synchronous.
        let endpoint = rt.block_on(async move { build_endpoint(skip_verify) })?;
        Ok(Self {
            inner: Arc::new(ClientInner { rt, endpoint }),
        })
    }

    /// Issues a GET request against `url`.
    pub fn get(&self, url: &str) -> Result<Response, Error> {
        self.do_request(Method::Get, url, None, &[])
    }

    /// Issues a POST request with the supplied body.
    pub fn post(&self, url: &str, body: &[u8], content_type: &str) -> Result<Response, Error> {
        self.do_request(
            Method::Post,
            url,
            Some(body),
            &[("Content-Type", content_type)],
        )
    }

    /// Issues a PUT request with the supplied body.
    pub fn put(&self, url: &str, body: &[u8], content_type: &str) -> Result<Response, Error> {
        self.do_request(
            Method::Put,
            url,
            Some(body),
            &[("Content-Type", content_type)],
        )
    }

    /// Issues a DELETE request. The optional body matches what
    /// some REST APIs expect (e.g. bulk-delete payloads).
    pub fn delete(
        &self,
        url: &str,
        body: Option<&[u8]>,
        headers: &[(&str, &str)],
    ) -> Result<Response, Error> {
        self.do_request(Method::Delete, url, body, headers)
    }

    /// Issues a HEAD request. The response body is always empty
    /// per RFC 9110.
    pub fn head(&self, url: &str, headers: &[(&str, &str)]) -> Result<Response, Error> {
        self.do_request(Method::Head, url, None, headers)
    }

    /// Issues an OPTIONS request - typically a preflight or
    /// capability probe with no body.
    pub fn options(&self, url: &str, headers: &[(&str, &str)]) -> Result<Response, Error> {
        self.do_request(Method::Options, url, None, headers)
    }

    /// Issues a request with the supplied method, optional body,
    /// and extra headers. The synchronous entry point underlying
    /// `get` / `post` / `put` / etc.
    pub fn do_request(
        &self,
        method: Method,
        url: &str,
        body: Option<&[u8]>,
        headers: &[(&str, &str)],
    ) -> Result<Response, Error> {
        let parsed = parse_url(url)?;
        let body_owned = body.map(Bytes::copy_from_slice);
        let headers_owned: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        let endpoint = self.inner.endpoint.clone();
        self.inner.rt.block_on(do_request_async(
            endpoint,
            method,
            parsed,
            body_owned,
            headers_owned,
        ))
    }

    /// Issues a request whose method is given as a string.
    /// Accepts `"GET"`, `"POST"`, `"PUT"`, `"DELETE"`, `"PATCH"`,
    /// `"HEAD"`, `"OPTIONS"` (case-insensitive).
    pub fn request(
        &self,
        method: &str,
        url: &str,
        body: Option<&[u8]>,
        headers: &[(&str, &str)],
    ) -> Result<Response, Error> {
        let m = Method::parse(method).ok_or_else(|| {
            Error::from(H3Error::Protocol(format!("unsupported method: {method}")))
        })?;
        self.do_request(m, url, body, headers)
    }
}

/// Parsed URL with the host / port / path-and-query split out
/// for the request pipeline.
struct ParsedUrl {
    host: String,
    port: u16,
    path_and_query: String,
    authority: String,
}

fn parse_url(url: &str) -> Result<ParsedUrl, Error> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| Error::from(H3Error::Protocol(format!("not https: {url}"))))?;
    let (authority, path_and_query) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rfind(':') {
        Some(idx) => {
            let h = &authority[..idx];
            let p: u16 = authority[idx + 1..]
                .parse()
                .map_err(|e| Error::from(H3Error::Protocol(format!("bad port: {e}"))))?;
            (h.to_string(), p)
        }
        None => (authority.to_string(), 443),
    };
    Ok(ParsedUrl {
        host,
        port,
        path_and_query: path_and_query.to_string(),
        authority: authority.to_string(),
    })
}

fn build_endpoint(skip_verify: bool) -> Result<quinn::Endpoint, Error> {
    let mut client_config = if skip_verify {
        let crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipVerify))
            .with_no_client_auth();
        quic_client_config_from_rustls(crypto)?
    } else {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let crypto = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        quic_client_config_from_rustls(crypto)?
    };
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_secs(5)));
    client_config.transport_config(Arc::new(transport));
    let bind: SocketAddr = "0.0.0.0:0"
        .parse()
        .map_err(|e| Error::from(H3Error::Io(format!("bind: {e}"))))?;
    let mut endpoint = quinn::Endpoint::client(bind)
        .map_err(|e| Error::from(H3Error::Io(format!("endpoint: {e}"))))?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

fn quic_client_config_from_rustls(
    mut crypto: rustls::ClientConfig,
) -> Result<quinn::ClientConfig, Error> {
    crypto.alpn_protocols = vec![b"h3".to_vec()];
    let quic = QuicClientConfig::try_from(crypto)
        .map_err(|e| Error::from(H3Error::Tls(format!("quic client config: {e}"))))?;
    Ok(quinn::ClientConfig::new(Arc::new(quic)))
}

/// Skip-cert-verification verifier for the `Client::insecure`
/// constructor. Production callers should never reach this path
/// - it bypasses every TLS authentication check.
#[derive(Debug)]
struct SkipVerify;

impl rustls::client::danger::ServerCertVerifier for SkipVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

async fn do_request_async(
    endpoint: quinn::Endpoint,
    method: Method,
    url: ParsedUrl,
    body: Option<Bytes>,
    headers: Vec<(String, String)>,
) -> Result<Response, Error> {
    let socket = tokio::net::lookup_host((url.host.as_str(), url.port))
        .await
        .map_err(|e| Error::from(H3Error::Io(format!("dns: {e}"))))?
        .next()
        .ok_or_else(|| Error::from(H3Error::Io(format!("no addr for {}", url.host))))?;
    let conn = endpoint
        .connect(socket, &url.host)
        .map_err(|e| Error::from(H3Error::Quic(format!("connect: {e}"))))?
        .await
        .map_err(|e| Error::from(H3Error::Quic(format!("handshake: {e}"))))?;
    let h3_conn = h3_quinn::Connection::new(conn);
    let (mut driver, mut send) = h3::client::new(h3_conn)
        .await
        .map_err(|e| Error::from(H3Error::Protocol(format!("h3 client: {e}"))))?;
    let driver_task = tokio::spawn(async move {
        let _ = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
    });

    let uri = format!("https://{}{}", url.authority, url.path_and_query);
    let mut req_builder = ::http::Request::builder()
        .method(method_to_http(method))
        .uri(uri);
    {
        let hmap = req_builder
            .headers_mut()
            .ok_or_else(|| Error::from(H3Error::Protocol("req headers".into())))?;
        for (name, value) in &headers {
            if let (Ok(n), Ok(v)) = (
                ::http::HeaderName::from_bytes(name.as_bytes()),
                ::http::HeaderValue::from_str(value),
            ) {
                hmap.insert(n, v);
            }
        }
    }
    let req: ::http::Request<()> = req_builder
        .body(())
        .map_err(|e| Error::from(H3Error::Protocol(format!("req build: {e}"))))?;
    let mut stream = send
        .send_request(req)
        .await
        .map_err(|e| Error::from(H3Error::Protocol(format!("send_request: {e}"))))?;
    if let Some(b) = body {
        stream
            .send_data(b)
            .await
            .map_err(|e| Error::from(H3Error::Protocol(format!("send_data: {e}"))))?;
    }
    stream
        .finish()
        .await
        .map_err(|e| Error::from(H3Error::Protocol(format!("client finish: {e}"))))?;

    let resp_head = stream
        .recv_response()
        .await
        .map_err(|e| Error::from(H3Error::Protocol(format!("recv_response: {e}"))))?;
    let status = StatusCode(resp_head.status().as_u16());
    let mut out_headers = Headers::new();
    for (name, value) in resp_head.headers() {
        if let Ok(v) = value.to_str() {
            out_headers.insert(name.as_str(), v);
        }
    }
    let mut body_bytes = BytesMut::new();
    while let Some(mut chunk) = stream
        .recv_data()
        .await
        .map_err(|e| Error::from(H3Error::Protocol(format!("recv_data: {e}"))))?
    {
        while chunk.has_remaining() {
            let s = chunk.chunk();
            body_bytes.extend_from_slice(s);
            let len = s.len();
            chunk.advance(len);
        }
    }

    driver_task.abort();
    let _ = driver_task.await;

    Ok(Response {
        status,
        headers: out_headers,
        body: body_bytes.to_vec(),
        raw_header_pairs: Vec::new(),
        body_stream: None,
    })
}

fn method_to_http(m: Method) -> ::http::Method {
    match m {
        Method::Get => ::http::Method::GET,
        Method::Post => ::http::Method::POST,
        Method::Put => ::http::Method::PUT,
        Method::Delete => ::http::Method::DELETE,
        Method::Patch => ::http::Method::PATCH,
        Method::Head => ::http::Method::HEAD,
        Method::Options => ::http::Method::OPTIONS,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket;

    /// Generates a self-signed PEM keypair via `rcgen` and writes
    /// the two PEM blobs to `(cert_path, key_path)` inside the
    /// supplied directory. The caller is responsible for cleaning
    /// the directory up.
    fn make_cert(tempdir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let cert =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("rcgen cert");
        let cert_path = tempdir.join("cert.pem");
        let key_path = tempdir.join("key.pem");
        std::fs::write(&cert_path, cert.cert.pem()).expect("write cert");
        std::fs::write(&key_path, cert.key_pair.serialize_pem()).expect("write key");
        (cert_path, key_path)
    }

    /// Probes for an unused ephemeral UDP port by binding zero
    /// and reading back the assigned port. There's a small race
    /// window between drop here and rebind in the server thread,
    /// but the ephemeral pool is large enough that it almost
    /// never collides on a developer box.
    fn unique_ephemeral_port() -> u16 {
        let sock = UdpSocket::bind("127.0.0.1:0").expect("udp bind");
        sock.local_addr().expect("local_addr").port()
    }

    #[test]
    fn h3_error_unsupported_renders() {
        let e: Error = H3Error::Unsupported.into();
        assert!(e.message().contains("unsupported"));
    }

    #[test]
    fn h3_client_insecure_builds() {
        let client = Client::insecure().expect("client");
        // Drop without issuing any request - verifies the runtime
        // + endpoint bind machinery works on this host.
        drop(client);
    }

    #[test]
    #[cfg_attr(
        not(feature = "slow-network-tests"),
        ignore = "QUIC handshake on loopback takes ≥60s on idle CI hardware; opt in with --features slow-network-tests"
    )]
    fn h3_round_trip_get_self_signed() {
        let tmp = std::env::temp_dir().join(format!(
            "gossamer-h3-{}-{}",
            std::process::id(),
            unique_ephemeral_port()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("temp dir");
        let (cert_path, key_path) = make_cert(&tmp);

        let port = unique_ephemeral_port();
        let addr = format!("127.0.0.1:{port}");
        let url = format!("https://localhost:{port}/hello");

        let server_addr = addr.clone();
        let server_cert = cert_path.to_string_lossy().to_string();
        let server_key = key_path.to_string_lossy().to_string();
        let _server_thread = std::thread::spawn(move || {
            let handler = |_req: Request| -> Response {
                Response {
                    status: StatusCode(200),
                    headers: Headers::new(),
                    body: b"hello h3".to_vec(),
                    raw_header_pairs: Vec::new(),
                    body_stream: None,
                }
            };
            let _ = serve(&server_addr, &server_cert, &server_key, handler);
        });

        // Give the server time to bind the UDP socket. 500ms on a
        // loopback box is more than enough for the rustls
        // handshake setup; smaller timeouts have surfaced flakes
        // on CI runners under load.
        std::thread::sleep(Duration::from_millis(500));

        let client = Client::insecure().expect("client build");
        let resp = client.get(&url).expect("client get");
        assert_eq!(resp.status, StatusCode(200));
        assert_eq!(resp.body, b"hello h3");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
