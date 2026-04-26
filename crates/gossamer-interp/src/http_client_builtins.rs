//! Interpreter hooks for `std::http::Client` (the GET-only slice
//! exercised by `examples/get_xkcd.gos`). HTTPS is not wired; a
//! program that hits an `https://` URL gets an `Err(...)` back.
//!
//! Kept in its own module so the main `builtins.rs` file stays
//! under the 2000-line hard limit defined in `GUIDELINES.md`.

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
// HTTP client builtins (examples/get_xkcd.gos)
//
// Minimal GET-only client over `std::net::TcpStream`.  HTTPS is
// unsupported; programs that hit it get `Err(...)` which
// `get_xkcd.gos` already handles gracefully.

pub(crate) fn builtin_http_client_new(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::struct_("Client".to_string(), Arc::new(Vec::new())))
}

pub(crate) fn builtin_http_client_get(args: &[Value]) -> RuntimeResult<Value> {
    let url = args.get(1).and_then(as_str).unwrap_or("");
    Ok(Value::struct_(
        "Request".to_string(),
        Arc::new(vec![(
            Ident::new("url"),
            Value::String(SmolStr::from(url.to_string())),
        )]),
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
        .unwrap_or("");
    match http_get(url) {
        Ok(response) => Ok(crate::builtins::ok_variant(response)),
        Err(err) => Ok(crate::builtins::err_variant(err)),
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
    Ok(Value::struct_("Response".to_string(), Arc::new(fields)))
}

fn http_get_plain(url: &str) -> Result<Value, String> {
    let (host, path) = parse_http_url(url).ok_or_else(|| format!("unsupported URL: {url}"))?;
    let (host_part, port) = match host.split_once(':') {
        Some((h, p)) => (h, p),
        None => (host.as_str(), "80"),
    };
    let address = format!("{host_part}:{port}");
    let mut stream = std::net::TcpStream::connect(&address)
        .map_err(|e| format!("connect {address}: {e}"))?;
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
    std::io::Read::read_to_end(&mut stream, &mut response)
        .map_err(|e| format!("read: {e}"))?;
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
    Ok(Value::struct_("Response".to_string(), Arc::new(fields)))
}

pub(crate) fn parse_http_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("http://")?;
    let (host, path) = match rest.split_once('/') {
        Some((h, p)) => (h.to_string(), format!("/{p}")),
        None => (rest.to_string(), "/".to_string()),
    };
    Some((host, path))
}
