//! Interpreter hooks for `std::http::Client`.
//!
//! - The legacy GET-only slice (`Client::new` / `Client::get` /
//!   `Request::send` for `method == "GET"`) is preserved verbatim:
//!   the existing TCP-only tests still route through the
//!   lightweight `gossamer_pkg::transport::HttpsTransport` path
//!   below.
//! - The richer client surface (`Client::post` / `put` / `options` /
//!   `delete` / `head`, plus the free functions `http::get`,
//!   `http::post`, `http::put`, `http::options`, `http::delete`,
//!   `http::head`, `http::request`, and `http::stream`) wraps
//!   `gossamer_std::http::Client` — ureq-backed: HTTPS via rustls
//!   (Mozilla roots), redirects, cookies, gzip decode, connection
//!   pool — all on the shared blocking I/O pool so per-goroutine
//!   workers stay free.
//!
//! Codegen note: only the bytecode VM dispatches the new method-
//! parameterised builtins by name. The Cranelift / LLVM tiers
//! still resolve `client.post(...)` etc. through the existing
//! `gos_rt_http_client_*` runtime symbols (GET-only today);
//! `http::request`, `http::stream`, and `ResponseStream::next_line`
//! have no native runtime helpers yet. Programs that exercise these
//! on the JIT / AOT tiers fall back to bytecode dispatch. Wiring
//! native helpers means extending
//! `gossamer-mir/src/lower.rs::intrinsic_runtime_call` and adding
//! matching entries to `gossamer-runtime/src/c_abi.rs`.
//!
//! Kept in its own module so the main `builtins.rs` file stays
//! under the 2000-line hard limit defined in `GUIDELINES.md`.

// Builtins return `RuntimeResult<Value>` to match the dispatcher's
// expected signature even when they never fail.
#![allow(clippy::unnecessary_wraps)]
use gossamer_pkg::transport::{HttpsTransport, Transport};
use std::sync::Arc;

use gossamer_ast::Ident;

use crate::value::{RuntimeError, RuntimeResult, SmolStr, Value};

fn as_str(value: &Value) -> Option<&str> {
    match value {
        Value::String(s) => Some(s.as_str()),
        _ => None,
    }
}

// ------------------------------------------------------------------
// HTTP client builtins
//
// Minimal GET-only client over `std::net::TcpStream`.  HTTPS is
// unsupported; programs that hit it get `Err(...)` which
// the test programs already handle gracefully.

pub(crate) fn builtin_http_client_new(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::struct_(
        "Client",
        Arc::unwrap_or_clone(crate::value::empty_struct_fields()),
    ))
}

pub(crate) fn builtin_http_client_get(args: &[Value]) -> RuntimeResult<Value> {
    let url = args.get(1).and_then(as_str).unwrap_or("");
    Ok(Value::struct_(
        "Request",
        vec![(
            Ident::new("url"),
            Value::String(SmolStr::from(url.to_string())),
        )],
    ))
}

pub(crate) fn builtin_http_request_send(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::Struct(inner)) = args.first() else {
        return Err(RuntimeError::Type(
            "Request::send: expected Request".to_string(),
        ));
    };
    if inner.name != "Request" {
        return Err(RuntimeError::Type(
            "Request::send: expected Request".to_string(),
        ));
    }
    let url = inner
        .fields
        .iter()
        .find(|(ident, _)| ident.name == "url")
        .and_then(|(_, v)| as_str(v))
        .unwrap_or("")
        .to_string();
    let method = inner
        .fields
        .iter()
        .find(|(ident, _)| ident.name == "method")
        .and_then(|(_, v)| as_str(v))
        .unwrap_or("GET")
        .to_string();
    if method.eq_ignore_ascii_case("GET") {
        // Legacy fast path: GET stays on the lightweight TCP/TLS
        // implementation that the early HTTP test programs used since
        // the start. The full ureq-backed path is only paid for when
        // the user actually picks a non-GET method.
        return match http_get(&url) {
            Ok(response) => Ok(crate::builtins::ok_variant(response)),
            Err(err) => Ok(crate::builtins::err_variant(err)),
        };
    }
    let Some(parsed) = gossamer_std::http::Method::parse(&method) else {
        return Ok(crate::builtins::err_variant(format!(
            "Request::send: unknown method `{method}`"
        )));
    };
    let client = gossamer_std::http::Client::new();
    match client.do_request(parsed, &url, None, &[]) {
        Ok(resp) => Ok(crate::builtins::ok_variant(lift_response(resp))),
        Err(e) => Ok(crate::builtins::err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_http_response_bytes(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::Struct(inner)) = args.first() else {
        return Err(RuntimeError::Type(
            "Response::bytes: expected Response".to_string(),
        ));
    };
    if inner.name != "Response" {
        return Err(RuntimeError::Type(
            "Response::bytes: expected Response".to_string(),
        ));
    }
    let body = inner
        .fields
        .iter()
        .find(|(ident, _)| ident.name == "body")
        .and_then(|(_, v)| as_str(v))
        .unwrap_or_default();
    let bytes: Vec<Value> = body.bytes().map(|b| Value::Int(i64::from(b))).collect();
    Ok(crate::builtins::ok_variant(Value::Array(Arc::new(bytes))))
}

/// Minimal HTTP(S) GET. HTTPS uses `gossamer-pkg`'s TLS transport;
/// HTTP uses a plain TCP socket. 3xx redirects (`301` / `302` /
/// `303` / `307` / `308`) are followed up to five hops, so callers
/// that hit the common `http://…` → `https://…` migration get the
/// final body instead of an empty redirect stub.
fn http_get(url: &str) -> Result<Value, String> {
    let mut current = url.to_string();
    for _ in 0..6 {
        let response = if current.starts_with("https://") {
            http_get_tls(&current)?
        } else {
            http_get_plain(&current)?
        };
        let Value::Struct(inner) = &response else {
            return Ok(response);
        };
        let status = inner
            .fields
            .iter()
            .find(|(ident, _)| ident.name == "status")
            .and_then(|(_, v)| match v {
                Value::Int(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(0);
        if !(300..=399).contains(&status) {
            return Ok(response);
        }
        let location = inner
            .fields
            .iter()
            .find(|(ident, _)| ident.name == "location")
            .and_then(|(_, v)| as_str(v));
        let Some(loc) = location else {
            return Ok(response);
        };
        current = absolute_redirect(&current, loc);
    }
    Err(format!("too many redirects fetching `{url}`"))
}

/// Resolves `location` against `from` when the redirect target is
/// relative (`/path`) rather than absolute (`https://host/...`).
pub(crate) fn absolute_redirect(from: &str, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        return location.to_string();
    }
    let scheme_end = from.find("://").map_or(0, |i| i + 3);
    let host_end = from[scheme_end..]
        .find('/')
        .map_or(from.len(), |i| scheme_end + i);
    if location.starts_with('/') {
        format!("{}{}", &from[..host_end], location)
    } else {
        format!("{}/{}", &from[..host_end], location)
    }
}

fn http_get_tls(url: &str) -> Result<Value, String> {
    let transport = HttpsTransport::new_mozilla_roots();
    let body = transport.get(url).map_err(|e| format!("{e}"))?;
    let body_str = String::from_utf8_lossy(&body).into_owned();
    let raw: Vec<Value> = body.iter().map(|b| Value::Int(i64::from(*b))).collect();
    let fields = vec![
        (Ident::new("status"), Value::Int(200)),
        (Ident::new("body"), Value::String(body_str.into())),
        (Ident::new("raw_bytes"), Value::Array(Arc::new(raw))),
        (
            Ident::new("content_type"),
            Value::String(SmolStr::from("text/plain".to_string())),
        ),
        (
            Ident::new("location"),
            Value::String(SmolStr::from(String::new())),
        ),
    ];
    Ok(Value::struct_(
        "Response",
        Arc::unwrap_or_clone(Arc::new(fields)),
    ))
}

fn http_get_plain(url: &str) -> Result<Value, String> {
    let (host, path) = parse_http_url(url).ok_or_else(|| format!("unsupported URL: {url}"))?;
    let (host_part, port) = match host.split_once(':') {
        Some((h, p)) => (h, p),
        None => (host.as_str(), "80"),
    };
    let address = format!("{host_part}:{port}");
    let mut stream =
        std::net::TcpStream::connect(&address).map_err(|e| format!("connect {address}: {e}"))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .ok();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_part}\r\nUser-Agent: gos/{version}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        version = env!("CARGO_PKG_VERSION"),
    );
    std::io::Write::write_all(&mut stream, request.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let mut response = Vec::new();
    std::io::Read::read_to_end(&mut stream, &mut response).map_err(|e| format!("read: {e}"))?;
    let response_str = String::from_utf8_lossy(&response);
    let Some((header_block, body)) = response_str.split_once("\r\n\r\n") else {
        return Err("invalid HTTP response".to_string());
    };
    let status_line = header_block.lines().next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    let mut location = String::new();
    for hline in header_block.lines().skip(1) {
        if let Some((name, value)) = hline.split_once(':') {
            if name.trim().eq_ignore_ascii_case("location") {
                location = value.trim().to_string();
                break;
            }
        }
    }
    let body_bytes: Vec<u8> = body.as_bytes().to_vec();
    let body_str = body.to_string();
    let raw: Vec<Value> = body_bytes
        .iter()
        .map(|b| Value::Int(i64::from(*b)))
        .collect();
    let fields = vec![
        (Ident::new("status"), Value::Int(status)),
        (Ident::new("body"), Value::String(body_str.into())),
        (Ident::new("raw_bytes"), Value::Array(Arc::new(raw))),
        (
            Ident::new("content_type"),
            Value::String(SmolStr::from("text/plain".to_string())),
        ),
        (Ident::new("location"), Value::String(location.into())),
    ];
    Ok(Value::struct_(
        "Response",
        Arc::unwrap_or_clone(Arc::new(fields)),
    ))
}

pub(crate) fn parse_http_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("http://")?;
    let (host, path) = match rest.split_once('/') {
        Some((h, p)) => (h.to_string(), format!("/{p}")),
        None => (rest.to_string(), "/".to_string()),
    };
    Some((host, path))
}

// ------------------------------------------------------------------
// HTTPS-capable client surface.
//
// Wraps `gossamer_std::http::Client` (ureq-backed: TLS via rustls,
// redirects, cookies, gzip, connection pool). Both the method-style
// API (`client.post(url, body)`) and the free-function API
// (`http::post(url, body, ct)`, `http::request(method, ...)`) route
// through the same `Client::do_request` core.
//
// `http::stream(method, url, body, headers)` returns a
// `ResponseStream { __handle, status, content_type }` whose
// `next_line()` reads one line at a time so SSE / chunked bodies
// don't need to be buffered in full.

use gossamer_std::http::{Client as StdClient, Method, StreamResponse};

/// Process-wide registry of in-flight streaming HTTP responses.
/// Indices into the vector are stable i64 handles embedded inside
/// the Gossamer `ResponseStream` value, so the reader can be looked
/// up across multiple `next_line` calls.
static STREAM_REGISTRY: parking_lot::Mutex<Vec<Option<Arc<parking_lot::Mutex<StreamResponse>>>>> =
    parking_lot::Mutex::new(Vec::new());

fn stream_register(stream: StreamResponse) -> i64 {
    let arc = Arc::new(parking_lot::Mutex::new(stream));
    let mut reg = STREAM_REGISTRY.lock();
    for (i, slot) in reg.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(arc);
            return i as i64;
        }
    }
    let id = reg.len() as i64;
    reg.push(Some(arc));
    id
}

fn stream_lookup(handle: i64) -> Option<Arc<parking_lot::Mutex<StreamResponse>>> {
    if handle < 0 {
        return None;
    }
    let reg = STREAM_REGISTRY.lock();
    reg.get(handle as usize).and_then(std::clone::Clone::clone)
}

fn handle_field(value: &Value, expected_name: &str) -> Option<i64> {
    let Value::Struct(inner) = value else {
        return None;
    };
    if inner.name != expected_name {
        return None;
    }
    for (ident, val) in &inner.fields {
        if ident.name == "__handle" {
            if let Value::Int(n) = val {
                return Some(*n);
            }
        }
    }
    None
}

/// Lifts a `gossamer_std::http::Response` into the same Gossamer
/// `Response` struct shape used by the legacy GET path
/// (`status`, `body`, `raw_bytes`, `content_type`, `location`).
fn lift_response(resp: gossamer_std::http::Response) -> Value {
    let body_str = String::from_utf8_lossy(&resp.body).into_owned();
    let raw: Vec<Value> = resp
        .body
        .iter()
        .map(|b| Value::Int(i64::from(*b)))
        .collect();
    let content_type = resp
        .headers
        .get("content-type")
        .unwrap_or("text/plain")
        .to_string();
    let location = resp.headers.get("location").unwrap_or("").to_string();
    let status = i64::from(resp.status.0);
    let fields = vec![
        (Ident::new("status"), Value::Int(status)),
        (Ident::new("body"), Value::String(body_str.into())),
        (Ident::new("raw_bytes"), Value::Array(Arc::new(raw))),
        (
            Ident::new("content_type"),
            Value::String(SmolStr::from(content_type)),
        ),
        (
            Ident::new("location"),
            Value::String(SmolStr::from(location)),
        ),
    ];
    Value::struct_("Response", Arc::unwrap_or_clone(Arc::new(fields)))
}

fn extract_header_pairs(value: Option<&Value>) -> Vec<(String, String)> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items.iter() {
        if let Value::Tuple(t) = item {
            if t.len() >= 2 {
                if let (Some(k), Some(v)) = (as_str(&t[0]), as_str(&t[1])) {
                    out.push((k.to_string(), v.to_string()));
                }
            }
        }
    }
    out
}

fn header_refs(pairs: &[(String, String)]) -> Vec<(&str, &str)> {
    pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

/// `http::request(method, url, body, headers) -> Result<Response, String>`.
pub(crate) fn builtin_http_request(args: &[Value]) -> RuntimeResult<Value> {
    let method_str = args.first().and_then(as_str).unwrap_or("");
    let Some(method) = Method::parse(method_str) else {
        return Ok(crate::builtins::err_variant(format!(
            "http::request: unknown method `{method_str}`"
        )));
    };
    let url = args.get(1).and_then(as_str).unwrap_or("");
    let body = args.get(2).and_then(as_str).unwrap_or("");
    let header_pairs = extract_header_pairs(args.get(3));
    let headers = header_refs(&header_pairs);
    let body_opt: Option<&[u8]> = if body.is_empty() {
        None
    } else {
        Some(body.as_bytes())
    };
    let client = StdClient::new();
    match client.do_request(method, url, body_opt, &headers) {
        Ok(resp) => Ok(crate::builtins::ok_variant(lift_response(resp))),
        Err(e) => Ok(crate::builtins::err_variant(format!("{e}"))),
    }
}

/// `http::get(url, headers) -> Result<Response, String>`.
pub(crate) fn builtin_http_get(args: &[Value]) -> RuntimeResult<Value> {
    let url = args.first().and_then(as_str).unwrap_or("");
    let header_pairs = extract_header_pairs(args.get(1));
    let headers = header_refs(&header_pairs);
    let client = StdClient::new();
    match client.do_request(Method::Get, url, None, &headers) {
        Ok(resp) => Ok(crate::builtins::ok_variant(lift_response(resp))),
        Err(e) => Ok(crate::builtins::err_variant(format!("{e}"))),
    }
}

/// `http::post(url, body, content_type) -> Result<Response, String>`.
pub(crate) fn builtin_http_post(args: &[Value]) -> RuntimeResult<Value> {
    let url = args.first().and_then(as_str).unwrap_or("");
    let body = args.get(1).and_then(as_str).unwrap_or("");
    let content_type = args.get(2).and_then(as_str).unwrap_or("application/json");
    let client = StdClient::new();
    match client.post(url, body.as_bytes(), content_type) {
        Ok(resp) => Ok(crate::builtins::ok_variant(lift_response(resp))),
        Err(e) => Ok(crate::builtins::err_variant(format!("{e}"))),
    }
}

/// `http::put(url, body, content_type) -> Result<Response, String>`.
pub(crate) fn builtin_http_put(args: &[Value]) -> RuntimeResult<Value> {
    let url = args.first().and_then(as_str).unwrap_or("");
    let body = args.get(1).and_then(as_str).unwrap_or("");
    let content_type = args.get(2).and_then(as_str).unwrap_or("application/json");
    let client = StdClient::new();
    match client.put(url, body.as_bytes(), content_type) {
        Ok(resp) => Ok(crate::builtins::ok_variant(lift_response(resp))),
        Err(e) => Ok(crate::builtins::err_variant(format!("{e}"))),
    }
}

/// `http::options(url, headers) -> Result<Response, String>`.
pub(crate) fn builtin_http_options(args: &[Value]) -> RuntimeResult<Value> {
    let url = args.first().and_then(as_str).unwrap_or("");
    let header_pairs = extract_header_pairs(args.get(1));
    let headers = header_refs(&header_pairs);
    let client = StdClient::new();
    match client.options(url, &headers) {
        Ok(resp) => Ok(crate::builtins::ok_variant(lift_response(resp))),
        Err(e) => Ok(crate::builtins::err_variant(format!("{e}"))),
    }
}

/// `http::delete(url, body, headers) -> Result<Response, String>`.
pub(crate) fn builtin_http_delete(args: &[Value]) -> RuntimeResult<Value> {
    let url = args.first().and_then(as_str).unwrap_or("");
    let body = args.get(1).and_then(as_str).unwrap_or("");
    let header_pairs = extract_header_pairs(args.get(2));
    let headers = header_refs(&header_pairs);
    let body_opt: Option<&[u8]> = if body.is_empty() {
        None
    } else {
        Some(body.as_bytes())
    };
    let client = StdClient::new();
    match client.delete(url, body_opt, &headers) {
        Ok(resp) => Ok(crate::builtins::ok_variant(lift_response(resp))),
        Err(e) => Ok(crate::builtins::err_variant(format!("{e}"))),
    }
}

/// `http::head(url, headers) -> Result<Response, String>`.
pub(crate) fn builtin_http_head(args: &[Value]) -> RuntimeResult<Value> {
    let url = args.first().and_then(as_str).unwrap_or("");
    let header_pairs = extract_header_pairs(args.get(1));
    let headers = header_refs(&header_pairs);
    let client = StdClient::new();
    match client.head(url, &headers) {
        Ok(resp) => Ok(crate::builtins::ok_variant(lift_response(resp))),
        Err(e) => Ok(crate::builtins::err_variant(format!("{e}"))),
    }
}

/// `http::stream(method, url, body, headers) -> Result<ResponseStream, String>`.
pub(crate) fn builtin_http_stream(args: &[Value]) -> RuntimeResult<Value> {
    let method_str = args.first().and_then(as_str).unwrap_or("");
    let Some(method) = Method::parse(method_str) else {
        return Ok(crate::builtins::err_variant(format!(
            "http::stream: unknown method `{method_str}`"
        )));
    };
    let url = args.get(1).and_then(as_str).unwrap_or("");
    let body = args.get(2).and_then(as_str).unwrap_or("");
    let header_pairs = extract_header_pairs(args.get(3));
    let headers = header_refs(&header_pairs);
    let body_opt: Option<&[u8]> = if body.is_empty() {
        None
    } else {
        Some(body.as_bytes())
    };
    let client = StdClient::new();
    match client.stream(method, url, body_opt, &headers) {
        Ok(stream) => {
            let status = i64::from(stream.status.0);
            let content_type = stream
                .headers
                .get("content-type")
                .unwrap_or("text/plain")
                .to_string();
            let handle = stream_register(stream);
            let fields = vec![
                (Ident::new("__handle"), Value::Int(handle)),
                (Ident::new("status"), Value::Int(status)),
                (
                    Ident::new("content_type"),
                    Value::String(SmolStr::from(content_type)),
                ),
            ];
            Ok(crate::builtins::ok_variant(Value::struct_(
                "ResponseStream",
                Arc::unwrap_or_clone(Arc::new(fields)),
            )))
        }
        Err(e) => Ok(crate::builtins::err_variant(format!("{e}"))),
    }
}

/// `ResponseStream::next_line() -> Option<String>`.
pub(crate) fn builtin_response_stream_next_line(args: &[Value]) -> RuntimeResult<Value> {
    let Some(handle) = args.first().and_then(|v| handle_field(v, "ResponseStream")) else {
        return Err(RuntimeError::Type(
            "ResponseStream::next_line: receiver must be ResponseStream".to_string(),
        ));
    };
    let Some(arc) = stream_lookup(handle) else {
        return Ok(crate::builtins::none_variant());
    };
    let mut guard = arc.lock();
    match guard.next_line() {
        Ok(Some(line)) => Ok(crate::builtins::some_variant(Value::String(line.into()))),
        Ok(None) | Err(_) => Ok(crate::builtins::none_variant()),
    }
}

// ------------------------------------------------------------------
// Method-style API: Client::post / put / options / delete / head /
// request — matching the existing `Client::get` call shape. The
// builder produces a `Request { method, url }` struct; calling
// `.send()` on it dispatches through `gossamer_std::http::Client`.

fn pending_request(method: &str, url: &str) -> Value {
    Value::struct_(
        "Request",
        vec![
            (
                Ident::new("method"),
                Value::String(SmolStr::from(method.to_string())),
            ),
            (
                Ident::new("url"),
                Value::String(SmolStr::from(url.to_string())),
            ),
        ],
    )
}

pub(crate) fn builtin_http_client_post(args: &[Value]) -> RuntimeResult<Value> {
    let url = args.get(1).and_then(as_str).unwrap_or("");
    Ok(pending_request("POST", url))
}

pub(crate) fn builtin_http_client_put(args: &[Value]) -> RuntimeResult<Value> {
    let url = args.get(1).and_then(as_str).unwrap_or("");
    Ok(pending_request("PUT", url))
}

pub(crate) fn builtin_http_client_options(args: &[Value]) -> RuntimeResult<Value> {
    let url = args.get(1).and_then(as_str).unwrap_or("");
    Ok(pending_request("OPTIONS", url))
}

pub(crate) fn builtin_http_client_delete(args: &[Value]) -> RuntimeResult<Value> {
    let url = args.get(1).and_then(as_str).unwrap_or("");
    Ok(pending_request("DELETE", url))
}

pub(crate) fn builtin_http_client_head(args: &[Value]) -> RuntimeResult<Value> {
    let url = args.get(1).and_then(as_str).unwrap_or("");
    Ok(pending_request("HEAD", url))
}
