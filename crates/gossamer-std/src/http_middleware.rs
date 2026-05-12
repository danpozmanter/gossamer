#![allow(
    clippy::similar_names,
    clippy::type_complexity,
    clippy::map_unwrap_or,
    clippy::redundant_closure,
    clippy::items_after_statements,
    clippy::let_and_return,
    clippy::needless_pass_by_value,
    clippy::doc_markdown,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args
)]

//! HTTP middleware suite.
//!
//! Each middleware is a function that wraps a [`Handler`] (the
//! router's handler type) and returns a new handler. Composition
//! is straightforward function chaining:
//!
//! ```no_run
//! use gossamer_std::http::{Response, StatusCode};
//! use gossamer_std::http_router::Router;
//! use gossamer_std::http_middleware as mw;
//!
//! let mut app = Router::new();
//! app.get("/", |_r, _p| Response::text(StatusCode::OK, "ok"));
//!
//! let _handler = mw::logger(mw::request_id(app));
//! ```
//!
//! The shipped middlewares:
//!
//! - [`logger`] — request/response logging.
//! - [`recoverer`] — catches handler panics, returns 500.
//! - [`request_id`] — stamps every response with `X-Request-Id`.
//! - [`cors`] — CORS preflight + per-response headers.
//! - [`basic_auth`] — HTTP Basic auth gate.
//! - [`compress_gzip`] — gzips response bodies when the client
//!   advertises `Accept-Encoding: gzip`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::http::{Headers, Request, Response, StatusCode};
use crate::http_router::{Params, Router};

/// Trait covering anything callable as a request handler. Both
/// `Router` and `Arc<dyn Fn(...)>` impl this.
pub trait Handler: Send + Sync + 'static {
    /// Invokes the handler.
    fn serve(&self, request: &Request, params: &Params) -> Response;
}

impl Handler for Router {
    fn serve(&self, request: &Request, _params: &Params) -> Response {
        Router::serve(self, request)
    }
}

impl<F> Handler for F
where
    F: Fn(&Request, &Params) -> Response + Send + Sync + 'static,
{
    fn serve(&self, request: &Request, params: &Params) -> Response {
        self(request, params)
    }
}

/// Wraps a `Router` in a chain of middleware. The wrapper itself
/// is a `Handler` so middleware compose naturally.
pub struct Chain<H: Handler> {
    inner: Arc<H>,
}

impl<H: Handler> Chain<H> {
    /// Creates a new chain wrapping `inner`.
    pub fn new(inner: H) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }
}

impl<H: Handler> Handler for Chain<H> {
    fn serve(&self, request: &Request, params: &Params) -> Response {
        self.inner.serve(request, params)
    }
}

// --- Logger ----------------------------------------------------------

/// Wraps `inner` with structured per-request logging. Each
/// request prints one line:
///
/// `<method> <path> <status> <bytes> <duration_ms>`
///
/// to stderr. Override the sink by replacing `slog`'s default
/// handler (the wrapper writes through `slog::info!`).
pub fn logger<H: Handler + 'static>(inner: H) -> impl Handler {
    let inner = Arc::new(inner);
    let wrapped = move |req: &Request, params: &Params| -> Response {
        let start = Instant::now();
        let resp = inner.serve(req, params);
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        eprintln!(
            "[http] {} {} {} {}b {:.3}ms",
            req.method.as_str(),
            req.path,
            resp.status.as_u16(),
            resp.body.len(),
            elapsed_ms
        );
        resp
    };
    wrapped
}

// --- Recoverer -------------------------------------------------------

/// Wraps `inner` with panic recovery. If the inner handler
/// panics, a `500 Internal Server Error` response is returned and
/// the panic message is logged to stderr. The panic is suppressed
/// — the server continues serving other requests.
pub fn recoverer<H: Handler + std::panic::RefUnwindSafe + 'static>(inner: H) -> impl Handler {
    let inner = Arc::new(inner);
    move |req: &Request, params: &Params| -> Response {
        let result = std::panic::catch_unwind(|| inner.serve(req, params));
        match result {
            Ok(r) => r,
            Err(payload) => {
                let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
                    (*s).to_string()
                } else if let Some(s) = payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "<non-string panic>".to_string()
                };
                eprintln!("[http] PANIC in handler: {msg}");
                let mut headers = Headers::new();
                headers.insert("content-type", "text/plain; charset=utf-8");
                Response {
                    status: StatusCode(500),
                    headers,
                    body: b"internal server error".to_vec(),
                }
            }
        }
    }
}

// --- Request-ID ------------------------------------------------------

static REQUEST_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Stamps each response with an `X-Request-Id` header. If the
/// incoming request already carries one, it is preserved (echoed
/// back on the response).
pub fn request_id<H: Handler + 'static>(inner: H) -> impl Handler {
    let inner = Arc::new(inner);
    move |req: &Request, params: &Params| -> Response {
        let id = req
            .headers
            .get("x-request-id")
            .map_or_else(|| next_request_id(), str::to_string);
        let mut resp = inner.serve(req, params);
        resp.headers.insert("x-request-id", &id);
        resp
    }
}

fn next_request_id() -> String {
    let n = REQUEST_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}-{n:x}")
}

// --- CORS ------------------------------------------------------------

/// CORS configuration.
#[derive(Clone, Debug)]
pub struct CorsConfig {
    /// Allowed origin (`Access-Control-Allow-Origin`). Use `*`
    /// for any.
    pub allow_origin: String,
    /// Methods accepted on preflight.
    pub allow_methods: Vec<String>,
    /// Headers accepted on preflight.
    pub allow_headers: Vec<String>,
    /// Whether to allow credentials.
    pub allow_credentials: bool,
    /// Max-age for preflight response in seconds.
    pub max_age: u32,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allow_origin: "*".to_string(),
            allow_methods: vec![
                "GET".into(),
                "POST".into(),
                "PUT".into(),
                "DELETE".into(),
                "OPTIONS".into(),
            ],
            allow_headers: vec!["content-type".into(), "authorization".into()],
            allow_credentials: false,
            max_age: 600,
        }
    }
}

/// CORS middleware. Handles preflight `OPTIONS` requests
/// inline; for normal requests it forwards to the inner handler
/// and decorates the response with the CORS headers.
pub fn cors<H: Handler + 'static>(config: CorsConfig, inner: H) -> impl Handler {
    let inner = Arc::new(inner);
    let cfg = config;
    move |req: &Request, params: &Params| -> Response {
        if req.method.as_str().eq_ignore_ascii_case("OPTIONS") {
            let mut headers = Headers::new();
            apply_cors_headers(&cfg, &mut headers);
            return Response {
                status: StatusCode(204),
                headers,
                body: Vec::new(),
            };
        }
        let mut resp = inner.serve(req, params);
        apply_cors_headers(&cfg, &mut resp.headers);
        resp
    }
}

fn apply_cors_headers(cfg: &CorsConfig, headers: &mut Headers) {
    headers.insert("access-control-allow-origin", &cfg.allow_origin);
    headers.insert(
        "access-control-allow-methods",
        &cfg.allow_methods.join(", "),
    );
    headers.insert(
        "access-control-allow-headers",
        &cfg.allow_headers.join(", "),
    );
    if cfg.allow_credentials {
        headers.insert("access-control-allow-credentials", "true");
    }
    headers.insert("access-control-max-age", &cfg.max_age.to_string());
}

// --- Basic auth ------------------------------------------------------

/// HTTP Basic authentication middleware. Returns `401
/// Unauthorized` with a `WWW-Authenticate` challenge when the
/// request does not carry valid credentials.
///
/// The `verify` closure receives `(username, password)` and
/// returns `true` to accept. Comparison should be
/// constant-time; the caller is responsible for that.
pub fn basic_auth<H, V>(realm: impl Into<String>, verify: V, inner: H) -> impl Handler
where
    H: Handler + 'static,
    V: Fn(&str, &str) -> bool + Send + Sync + 'static,
{
    let inner = Arc::new(inner);
    let realm = realm.into();
    move |req: &Request, params: &Params| -> Response {
        let header = req.headers.get("authorization").unwrap_or("");
        if let Some(rest) = header
            .strip_prefix("Basic ")
            .or_else(|| header.strip_prefix("basic "))
        {
            if let Some((user, pass)) = decode_basic(rest.trim())
                && verify(&user, &pass)
            {
                return inner.serve(req, params);
            }
        }
        let mut headers = Headers::new();
        headers.insert(
            "www-authenticate",
            &format!("Basic realm=\"{}\", charset=\"UTF-8\"", realm),
        );
        Response {
            status: StatusCode(401),
            headers,
            body: b"unauthorized".to_vec(),
        }
    }
}

fn decode_basic(b64: &str) -> Option<(String, String)> {
    let decoded = base64_decode(b64.trim()).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (user, pass) = text.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

fn base64_decode(input: &str) -> Result<Vec<u8>, ()> {
    // Standard RFC 4648 base64 (no URL-safe variant). Built
    // inline to avoid a dependency on the std::encoding feature
    // which is gated.
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for ch in input.chars() {
        if ch == '=' {
            break;
        }
        let v: u32 = match ch {
            'A'..='Z' => u32::from(ch as u8 - b'A'),
            'a'..='z' => u32::from(ch as u8 - b'a') + 26,
            '0'..='9' => u32::from(ch as u8 - b'0') + 52,
            '+' => 62,
            '/' => 63,
            ' ' | '\r' | '\n' | '\t' => continue,
            _ => return Err(()),
        };
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

// --- Compress (gzip) -------------------------------------------------

/// Gzip-compresses the response body when the client advertises
/// `Accept-Encoding: gzip`. Sets `Content-Encoding: gzip` and
/// recalculates `Content-Length`. Bodies below `min_bytes` are
/// emitted uncompressed (the framing overhead isn't worth it).
///
/// This middleware depends on the `compress` Cargo feature
/// (flate2). Without the feature, it's a no-op pass-through.
pub fn compress_gzip<H: Handler + 'static>(min_bytes: usize, inner: H) -> impl Handler {
    let inner = Arc::new(inner);
    move |req: &Request, params: &Params| -> Response {
        #[allow(unused_mut)]
        let mut resp = inner.serve(req, params);
        if resp.body.len() < min_bytes {
            return resp;
        }
        let accept = req.headers.get("accept-encoding").unwrap_or("");
        if !accept
            .split(',')
            .any(|tok| tok.trim().eq_ignore_ascii_case("gzip"))
        {
            return resp;
        }
        {
            use std::io::Write;
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            if encoder.write_all(&resp.body).is_ok()
                && let Ok(compressed) = encoder.finish()
            {
                resp.body = compressed;
                resp.headers.insert("content-encoding", "gzip");
                resp.headers.remove("content-length");
                let len = resp.body.len().to_string();
                resp.headers.insert("content-length", &len);
            }
        }
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use crate::http::Method;

    fn req(method: Method, path: &str) -> Request {
        Request {
            method,
            path: path.to_string(),
            query: String::new(),
            headers: Headers::new(),
            body: Vec::new(),
            context: Context::background(),
        }
    }

    fn text_response(status: u16, body: &str) -> Response {
        Response {
            status: StatusCode(status),
            headers: Headers::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn logger_passes_response_through() {
        let inner = |_req: &Request, _p: &Params| text_response(200, "ok");
        let wrapped = logger(inner);
        let resp = wrapped.serve(&req(Method::Get, "/x"), &Params::default());
        assert_eq!(resp.status, StatusCode(200));
        assert_eq!(resp.body, b"ok");
    }

    #[test]
    fn recoverer_catches_panic_returns_500() {
        let inner = |_req: &Request, _p: &Params| -> Response {
            panic!("handler exploded");
        };
        let wrapped = recoverer(inner);
        let resp = wrapped.serve(&req(Method::Get, "/x"), &Params::default());
        assert_eq!(resp.status, StatusCode(500));
        assert_eq!(resp.body, b"internal server error");
    }

    #[test]
    fn recoverer_passes_clean_response_through() {
        let inner = |_req: &Request, _p: &Params| text_response(201, "created");
        let wrapped = recoverer(inner);
        let resp = wrapped.serve(&req(Method::Post, "/x"), &Params::default());
        assert_eq!(resp.status, StatusCode(201));
        assert_eq!(resp.body, b"created");
    }

    #[test]
    fn request_id_stamps_response_header() {
        let inner = |_req: &Request, _p: &Params| text_response(200, "");
        let wrapped = request_id(inner);
        let resp = wrapped.serve(&req(Method::Get, "/x"), &Params::default());
        assert!(resp.headers.get("x-request-id").is_some());
    }

    #[test]
    fn request_id_preserves_incoming() {
        let inner = |_req: &Request, _p: &Params| text_response(200, "");
        let wrapped = request_id(inner);
        let mut r = req(Method::Get, "/x");
        r.headers.insert("x-request-id", "custom-id-123");
        let resp = wrapped.serve(&r, &Params::default());
        assert_eq!(resp.headers.get("x-request-id"), Some("custom-id-123"));
    }

    #[test]
    fn cors_preflight_returns_204_with_headers() {
        let inner = |_req: &Request, _p: &Params| text_response(200, "real");
        let wrapped = cors(CorsConfig::default(), inner);
        let resp = wrapped.serve(&req(Method::Options, "/x"), &Params::default());
        assert_eq!(resp.status, StatusCode(204));
        assert_eq!(resp.headers.get("access-control-allow-origin"), Some("*"));
        assert!(resp.headers.get("access-control-allow-methods").is_some());
    }

    #[test]
    fn cors_decorates_non_preflight_response() {
        let inner = |_req: &Request, _p: &Params| text_response(200, "ok");
        let wrapped = cors(CorsConfig::default(), inner);
        let resp = wrapped.serve(&req(Method::Get, "/x"), &Params::default());
        assert_eq!(resp.body, b"ok");
        assert_eq!(resp.headers.get("access-control-allow-origin"), Some("*"));
    }

    #[test]
    fn basic_auth_rejects_missing_credentials() {
        let inner = |_req: &Request, _p: &Params| text_response(200, "secret");
        let wrapped = basic_auth(
            "test-realm",
            |u, p| u == "alice" && p == "wonderland",
            inner,
        );
        let resp = wrapped.serve(&req(Method::Get, "/x"), &Params::default());
        assert_eq!(resp.status, StatusCode(401));
        let challenge = resp.headers.get("www-authenticate").unwrap_or("");
        assert!(challenge.contains("test-realm"));
    }

    #[test]
    fn basic_auth_accepts_correct_credentials() {
        let inner = |_req: &Request, _p: &Params| text_response(200, "secret");
        let wrapped = basic_auth("test", |u, p| u == "alice" && p == "wonderland", inner);
        let mut r = req(Method::Get, "/x");
        // base64("alice:wonderland") = "YWxpY2U6d29uZGVybGFuZA=="
        r.headers
            .insert("authorization", "Basic YWxpY2U6d29uZGVybGFuZA==");
        let resp = wrapped.serve(&r, &Params::default());
        assert_eq!(resp.status, StatusCode(200));
        assert_eq!(resp.body, b"secret");
    }

    #[test]
    fn basic_auth_rejects_wrong_password() {
        let inner = |_req: &Request, _p: &Params| text_response(200, "secret");
        let wrapped = basic_auth("test", |u, p| u == "alice" && p == "wonderland", inner);
        let mut r = req(Method::Get, "/x");
        // base64("alice:wrong") = "YWxpY2U6d3Jvbmc="
        r.headers.insert("authorization", "Basic YWxpY2U6d3Jvbmc=");
        let resp = wrapped.serve(&r, &Params::default());
        assert_eq!(resp.status, StatusCode(401));
    }

    #[test]
    fn base64_round_trip_minimal_vectors() {
        // Sanity-check the inline base64 decoder.
        assert_eq!(base64_decode("YWJj").unwrap(), b"abc");
        assert_eq!(base64_decode("YWI=").unwrap(), b"ab");
        assert_eq!(base64_decode("YQ==").unwrap(), b"a");
        assert_eq!(base64_decode("").unwrap(), b"");
    }

    #[test]
    fn compress_gzip_encodes_when_accepted_and_above_threshold() {
        let payload: Vec<u8> = (0..2048).map(|i| (i & 0xff) as u8).collect();
        let payload_cloned = payload.clone();
        let inner = move |_req: &Request, _p: &Params| Response {
            status: StatusCode(200),
            headers: Headers::new(),
            body: payload_cloned.clone(),
        };
        let wrapped = compress_gzip(64, inner);
        let mut r = req(Method::Get, "/x");
        r.headers.insert("accept-encoding", "gzip, deflate");
        let resp = wrapped.serve(&r, &Params::default());
        assert_eq!(resp.headers.get("content-encoding"), Some("gzip"));
        assert!(resp.body.len() < payload.len(), "expected compression");
        // Decode and verify round-trip.
        use std::io::Read;
        let mut decoder = flate2::read::GzDecoder::new(&resp.body[..]);
        let mut decoded = Vec::new();
        decoder.read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn compress_gzip_skips_below_threshold() {
        let inner = |_req: &Request, _p: &Params| text_response(200, "small");
        let wrapped = compress_gzip(64, inner);
        let mut r = req(Method::Get, "/x");
        r.headers.insert("accept-encoding", "gzip");
        let resp = wrapped.serve(&r, &Params::default());
        assert!(resp.headers.get("content-encoding").is_none());
        assert_eq!(resp.body, b"small");
    }

    #[test]
    fn compress_gzip_skips_when_not_accepted() {
        let payload: Vec<u8> = vec![0u8; 4096];
        let payload_cloned = payload.clone();
        let inner = move |_req: &Request, _p: &Params| Response {
            status: StatusCode(200),
            headers: Headers::new(),
            body: payload_cloned.clone(),
        };
        let wrapped = compress_gzip(64, inner);
        let resp = wrapped.serve(&req(Method::Get, "/x"), &Params::default());
        assert!(resp.headers.get("content-encoding").is_none());
        assert_eq!(resp.body.len(), payload.len());
    }
}
