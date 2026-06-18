//! QUIC + HTTP/3 (RFC 9114) engine shared by the Gossamer
//! interpreter and the compiled-tier runtime.
//!
//! Both `quinn` and `h3` are async-only and assume a tokio reactor
//! is driving timers and UDP I/O. Gossamer's scheduler does not
//! expose those primitives, so each [`serve`] call and each
//! [`Client`] instance spins up its own current-thread tokio
//! runtime that stays private to this crate; callers see only
//! synchronous entry points.
//!
//! The engine speaks plain wire types ([`H3Request`] /
//! [`H3Response`]) rather than any tier's `http::Request` /
//! `http::Response`, so the same code drives the interpreter's
//! `Value`-shaped handler and the compiled tier's C-ABI handler
//! without either tier's types leaking into the QUIC layer.

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
}

/// A request as it arrives off the wire, before any tier-specific
/// representation is built. `path` and `query` are already split;
/// `headers` carry the raw HTTP/3 pseudo-header-free header list
/// in arrival order.
pub struct H3Request {
    /// Uppercase HTTP method (`GET`, `POST`, ...).
    pub method: String,
    /// Path component of the request target (no query string).
    pub path: String,
    /// Query string without the leading `?`, empty when absent.
    pub query: String,
    /// Header name/value pairs in arrival order; names lowercase
    /// per HTTP/3 framing.
    pub headers: Vec<(String, String)>,
    /// Fully buffered request body.
    pub body: Vec<u8>,
}

/// A complete response handed back from a handler.
pub struct H3Response {
    /// HTTP status code.
    pub status: u16,
    /// Header name/value pairs to emit, in order.
    pub headers: Vec<(String, String)>,
    /// Response body bytes.
    pub body: Vec<u8>,
}

impl H3Response {
    /// Builds the canonical `500 internal server error` response a
    /// panicking or absent handler resolves to.
    #[must_use]
    pub fn internal_error() -> Self {
        Self {
            status: 500,
            headers: Vec::new(),
            body: b"internal server error".to_vec(),
        }
    }
}

/// Per-request handler. A blanket impl covers any
/// `Fn(H3Request) -> H3Response`, so both the interpreter adapter
/// and the compiled-tier adapter pass plain closures.
pub trait Handler: Send + Sync + 'static {
    /// Serves one HTTP/3 request.
    fn handle(&self, request: H3Request) -> H3Response;
}

impl<F> Handler for F
where
    F: Fn(H3Request) -> H3Response + Send + Sync + 'static,
{
    fn handle(&self, request: H3Request) -> H3Response {
        self(request)
    }
}

/// Installs the rustls ring crypto provider exactly once for this
/// process. Idempotent - both server and client entry points call
/// this before touching any rustls configuration.
fn install_ring_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Splits a request target into `(path, query)` at the first `?`.
/// The query is returned without the leading `?`; an absent query
/// yields an empty string.
fn split_path_query(target: &str) -> (String, String) {
    match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (target.to_string(), String::new()),
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Reads the PEM keypair at `cert_path` / `key_path` and runs
/// [`serve`]. Reading the keypair here - rather than in each tier's
/// adapter - keeps the cert / key error wording identical across the
/// interpreter and the compiled runtime, which both reach this entry
/// with the same file paths.
pub fn serve_files<H>(
    addr: &str,
    cert_path: &str,
    key_path: &str,
    handler: H,
) -> Result<(), H3Error>
where
    H: Handler,
{
    let cert_pem = std::fs::read(cert_path).map_err(|e| H3Error::Io(format!("read cert: {e}")))?;
    let key_pem = std::fs::read(key_path).map_err(|e| H3Error::Io(format!("read key: {e}")))?;
    serve(addr, &cert_pem, &key_pem, handler)
}

/// Binds a UDP socket on `addr`, runs a QUIC + HTTP/3 endpoint, and
/// dispatches every accepted request to `handler`. Blocks the
/// calling thread until the endpoint stops accepting.
///
/// `cert_pem` / `key_pem` are PEM-encoded bytes. HTTP/3 mandates
/// TLS, so a keypair is required. A bind / TLS / config failure is
/// reported synchronously as an `Err` before the accept loop runs,
/// so callers can surface it the same way the plaintext server
/// surfaces a bind failure.
pub fn serve<H>(addr: &str, cert_pem: &[u8], key_pem: &[u8], handler: H) -> Result<(), H3Error>
where
    H: Handler,
{
    install_ring_provider();
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<_, _>>()
        .map_err(|e| H3Error::Tls(format!("cert parse: {e}")))?;
    if certs.is_empty() {
        return Err(H3Error::Tls("no certificates in PEM".into()));
    }
    let key: PrivateKeyDer<'static> = PrivateKeyDer::pem_slice_iter(key_pem)
        .next()
        .ok_or_else(|| H3Error::Tls("no private key in PEM".into()))?
        .map_err(|e| H3Error::Tls(format!("key parse: {e}")))?;

    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| H3Error::Tls(format!("server config: {e}")))?;
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let quic_server = QuicServerConfig::try_from(tls)
        .map_err(|e| H3Error::Tls(format!("quic server config: {e}")))?;
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_server));
    {
        let transport = Arc::get_mut(&mut server_config.transport)
            .ok_or_else(|| H3Error::Quic("transport config aliased".into()))?;
        transport.max_concurrent_uni_streams(0_u8.into());
    }

    let socket_addr: SocketAddr = addr
        .parse()
        .map_err(|e| H3Error::Io(format!("parse addr: {e}")))?;

    let rt = RtBuilder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| H3Error::Io(format!("tokio runtime: {e}")))?;
    let handler = Arc::new(handler);
    rt.block_on(run_server(socket_addr, server_config, handler))
}

async fn run_server<H>(
    addr: SocketAddr,
    config: quinn::ServerConfig,
    handler: Arc<H>,
) -> Result<(), H3Error>
where
    H: Handler,
{
    let endpoint = quinn::Endpoint::server(config, addr)
        .map_err(|e| H3Error::Io(format!("endpoint bind: {e}")))?;
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

async fn serve_connection<H>(incoming: quinn::Incoming, handler: Arc<H>) -> Result<(), H3Error>
where
    H: Handler,
{
    let conn = incoming
        .await
        .map_err(|e| H3Error::Quic(format!("accept: {e}")))?;
    let h3_conn = h3_quinn::Connection::new(conn);
    let mut h3_server = h3::server::Connection::<_, Bytes>::new(h3_conn)
        .await
        .map_err(|e| H3Error::Protocol(format!("h3 conn: {e}")))?;

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
                return Err(H3Error::Protocol(format!("accept stream: {e}")));
            }
        }
    }
    Ok(())
}

async fn serve_stream<C, H>(
    resolver: h3::server::RequestResolver<C, Bytes>,
    handler: Arc<H>,
) -> Result<(), H3Error>
where
    C: h3::quic::Connection<Bytes>,
    H: Handler,
{
    let (req, mut stream) = resolver
        .resolve_request()
        .await
        .map_err(|e| H3Error::Protocol(format!("resolve: {e}")))?;

    let (parts, ()) = req.into_parts();
    let method = parts.method.as_str().to_string();
    let path_q = parts
        .uri
        .path_and_query()
        .map_or_else(|| "/".to_string(), |pq| pq.as_str().to_string());
    let (path, query) = split_path_query(&path_q);

    let mut headers = Vec::new();
    for (name, value) in &parts.headers {
        if let Ok(v) = value.to_str() {
            headers.push((name.as_str().to_string(), v.to_string()));
        }
    }

    let mut body = Vec::<u8>::new();
    while let Some(mut chunk) = stream
        .recv_data()
        .await
        .map_err(|e| H3Error::Protocol(format!("recv body: {e}")))?
    {
        while chunk.has_remaining() {
            let bs = chunk.chunk();
            body.extend_from_slice(bs);
            let len = bs.len();
            chunk.advance(len);
        }
    }

    let request = H3Request {
        method,
        path,
        query,
        headers,
        body,
    };

    let response =
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler.handle(request))) {
            Ok(resp) => resp,
            Err(_) => H3Response::internal_error(),
        };

    let status = ::http::StatusCode::from_u16(response.status)
        .map_err(|e| H3Error::Protocol(format!("bad status: {e}")))?;
    let mut builder = ::http::Response::builder().status(status);
    {
        let h = builder
            .headers_mut()
            .ok_or_else(|| H3Error::Protocol("response head".into()))?;
        for (name, value) in &response.headers {
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
        .map_err(|e| H3Error::Protocol(format!("build head: {e}")))?;
    stream
        .send_response(head)
        .await
        .map_err(|e| H3Error::Protocol(format!("send_response: {e}")))?;
    if !response.body.is_empty() {
        stream
            .send_data(Bytes::from(response.body))
            .await
            .map_err(|e| H3Error::Protocol(format!("send_data: {e}")))?;
    }
    stream
        .finish()
        .await
        .map_err(|e| H3Error::Protocol(format!("finish: {e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// A response collected from an HTTP/3 client request.
pub struct ClientResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response header name/value pairs in arrival order.
    pub headers: Vec<(String, String)>,
    /// Fully buffered response body.
    pub body: Vec<u8>,
}

/// HTTP/3 client. Each instance owns a private tokio runtime (one
/// worker thread) plus a `quinn::Endpoint` bound to an ephemeral
/// UDP port. Cloning is O(1); internal state is `Arc`-shared.
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
    pub fn new() -> Result<Self, H3Error> {
        Self::build(false)
    }

    /// Constructs a client that accepts any server certificate.
    /// Intended for tests and self-signed development endpoints -
    /// never use in production.
    pub fn insecure() -> Result<Self, H3Error> {
        Self::build(true)
    }

    fn build(skip_verify: bool) -> Result<Self, H3Error> {
        install_ring_provider();
        let rt = RtBuilder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| H3Error::Io(format!("tokio runtime: {e}")))?;
        // `quinn::Endpoint::client` must be called from inside a
        // tokio runtime so its background driver task can be
        // spawned; `block_on` keeps the constructor synchronous.
        let endpoint = rt.block_on(async move { build_endpoint(skip_verify) })?;
        Ok(Self {
            inner: Arc::new(ClientInner { rt, endpoint }),
        })
    }

    /// Issues a request with the supplied method, optional body, and
    /// extra headers against `url` (which must be `https://...`).
    pub fn request(
        &self,
        method: &str,
        url: &str,
        body: Option<&[u8]>,
        headers: &[(String, String)],
    ) -> Result<ClientResponse, H3Error> {
        let parsed = parse_url(url)?;
        let method = ::http::Method::from_bytes(method.as_bytes())
            .map_err(|e| H3Error::Protocol(format!("bad method: {e}")))?;
        let body_owned = body.map(Bytes::copy_from_slice);
        let headers_owned = headers.to_vec();
        let endpoint = self.inner.endpoint.clone();
        self.inner.rt.block_on(do_request_async(
            endpoint,
            method,
            parsed,
            body_owned,
            headers_owned,
        ))
    }
}

/// Parsed URL with the host / port / path-and-query split out for
/// the request pipeline.
struct ParsedUrl {
    host: String,
    port: u16,
    path_and_query: String,
    authority: String,
}

fn parse_url(url: &str) -> Result<ParsedUrl, H3Error> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| H3Error::Protocol(format!("not https: {url}")))?;
    let (authority, path_and_query) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rfind(':') {
        Some(idx) => {
            let h = &authority[..idx];
            let p: u16 = authority[idx + 1..]
                .parse()
                .map_err(|e| H3Error::Protocol(format!("bad port: {e}")))?;
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

fn build_endpoint(skip_verify: bool) -> Result<quinn::Endpoint, H3Error> {
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
        .map_err(|e| H3Error::Io(format!("bind: {e}")))?;
    let mut endpoint =
        quinn::Endpoint::client(bind).map_err(|e| H3Error::Io(format!("endpoint: {e}")))?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

fn quic_client_config_from_rustls(
    mut crypto: rustls::ClientConfig,
) -> Result<quinn::ClientConfig, H3Error> {
    crypto.alpn_protocols = vec![b"h3".to_vec()];
    let quic = QuicClientConfig::try_from(crypto)
        .map_err(|e| H3Error::Tls(format!("quic client config: {e}")))?;
    Ok(quinn::ClientConfig::new(Arc::new(quic)))
}

/// Skip-cert-verification verifier for the [`Client::insecure`]
/// constructor. Production callers should never reach this path -
/// it bypasses every TLS authentication check.
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
    method: ::http::Method,
    url: ParsedUrl,
    body: Option<Bytes>,
    headers: Vec<(String, String)>,
) -> Result<ClientResponse, H3Error> {
    let socket = tokio::net::lookup_host((url.host.as_str(), url.port))
        .await
        .map_err(|e| H3Error::Io(format!("dns: {e}")))?
        .next()
        .ok_or_else(|| H3Error::Io(format!("no addr for {}", url.host)))?;
    let conn = endpoint
        .connect(socket, &url.host)
        .map_err(|e| H3Error::Quic(format!("connect: {e}")))?
        .await
        .map_err(|e| H3Error::Quic(format!("handshake: {e}")))?;
    let h3_conn = h3_quinn::Connection::new(conn);
    let (mut driver, mut send) = h3::client::new(h3_conn)
        .await
        .map_err(|e| H3Error::Protocol(format!("h3 client: {e}")))?;
    let driver_task = tokio::spawn(async move {
        let _ = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
    });

    let uri = format!("https://{}{}", url.authority, url.path_and_query);
    let mut req_builder = ::http::Request::builder().method(method).uri(uri);
    {
        let hmap = req_builder
            .headers_mut()
            .ok_or_else(|| H3Error::Protocol("req headers".into()))?;
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
        .map_err(|e| H3Error::Protocol(format!("req build: {e}")))?;
    let mut stream = send
        .send_request(req)
        .await
        .map_err(|e| H3Error::Protocol(format!("send_request: {e}")))?;
    if let Some(b) = body {
        stream
            .send_data(b)
            .await
            .map_err(|e| H3Error::Protocol(format!("send_data: {e}")))?;
    }
    stream
        .finish()
        .await
        .map_err(|e| H3Error::Protocol(format!("client finish: {e}")))?;

    let resp_head = stream
        .recv_response()
        .await
        .map_err(|e| H3Error::Protocol(format!("recv_response: {e}")))?;
    let status = resp_head.status().as_u16();
    let mut out_headers = Vec::new();
    for (name, value) in resp_head.headers() {
        if let Ok(v) = value.to_str() {
            out_headers.push((name.as_str().to_string(), v.to_string()));
        }
    }
    let mut body_bytes = BytesMut::new();
    while let Some(mut chunk) = stream
        .recv_data()
        .await
        .map_err(|e| H3Error::Protocol(format!("recv_data: {e}")))?
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

    Ok(ClientResponse {
        status,
        headers: out_headers,
        body: body_bytes.to_vec(),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket;

    /// Generates a self-signed PEM keypair via `rcgen` and returns
    /// the `(cert_pem, key_pem)` byte blobs.
    fn make_cert() -> (Vec<u8>, Vec<u8>) {
        let cert =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("rcgen cert");
        (
            cert.cert.pem().into_bytes(),
            cert.signing_key.serialize_pem().into_bytes(),
        )
    }

    /// Probes for an unused ephemeral UDP port by binding zero and
    /// reading back the assigned port.
    fn unique_ephemeral_port() -> u16 {
        let sock = UdpSocket::bind("127.0.0.1:0").expect("udp bind");
        sock.local_addr().expect("local_addr").port()
    }

    #[test]
    fn serve_rejects_unparseable_address_synchronously() {
        let (cert, key) = make_cert();
        let err = serve("not-an-address", &cert, &key, |_req: H3Request| {
            H3Response {
                status: 200,
                headers: Vec::new(),
                body: b"ok".to_vec(),
            }
        })
        .expect_err("unparseable address must fail before the accept loop");
        assert!(matches!(err, H3Error::Io(_)), "got {err:?}");
    }

    #[test]
    fn serve_rejects_empty_cert_pem() {
        let (_, key) = make_cert();
        let err = serve("127.0.0.1:0", b"", &key, |_req: H3Request| {
            H3Response::internal_error()
        })
        .expect_err("empty cert PEM must fail");
        assert!(matches!(err, H3Error::Tls(_)), "got {err:?}");
    }

    #[test]
    fn h3_client_insecure_builds() {
        let client = Client::insecure().expect("client");
        drop(client);
    }

    #[test]
    #[cfg_attr(
        not(feature = "slow-network-tests"),
        ignore = "QUIC handshake on loopback takes >=60s on idle CI hardware; opt in with --features slow-network-tests"
    )]
    fn h3_round_trip_get_self_signed() {
        let (cert, key) = make_cert();
        let port = unique_ephemeral_port();
        let addr = format!("127.0.0.1:{port}");
        let url = format!("https://localhost:{port}/hello");

        let server_addr = addr.clone();
        let _server_thread = std::thread::spawn(move || {
            let _ = serve(&server_addr, &cert, &key, |_req: H3Request| H3Response {
                status: 200,
                headers: Vec::new(),
                body: b"hello h3".to_vec(),
            });
        });

        std::thread::sleep(Duration::from_millis(500));

        let client = Client::insecure().expect("client build");
        let resp = client.request("GET", &url, None, &[]).expect("client get");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"hello h3");
    }
}
