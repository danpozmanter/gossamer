#![allow(
    unused_imports,
    dead_code,
    clippy::wildcard_imports,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::needless_range_loop,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::doc_markdown,
    clippy::module_name_repetitions
)]
//! Bytecode-VM builtins for the stateless string/crypto surface of
//! `std::http::cookie`, `std::http::csrf`, and `std::http::session`.
//! These mirror the compiled-tier shims in
//! `gossamer-runtime/src/c_abi/http_security.rs` so `gos run`,
//! Cranelift, and LLVM produce bit-identical output.
//!
//! `cookie::*` and `csrf::*` delegate to `gossamer_std::http_{cookie,
//! csrf}`; `session::sign` / `session::verify` reimplement the
//! `SignedCookieStore` SignedOnly wire format inline (the store's
//! encode/decode are private methods) on top of the public
//! `gossamer_std::crypto::hmac::sha256_mac` primitive.

use std::sync::Arc;

use crate::builtins::{BuiltinFnPub, as_str, err_variant, ok_variant};
use crate::value::{RuntimeResult, Value};

use super::*;

/// Entry point invoked from `stdlib_builtins::install`.
pub(crate) fn install_http_security(globals: &mut Vec<(&'static str, Value)>) {
    for (path, call) in [
        (
            "http::cookie::parse_cookie_header",
            builtin_cookie_parse_header as BuiltinFnPub,
        ),
        ("http::cookie::serialize", builtin_cookie_serialize),
        ("http::csrf::issue_token", builtin_csrf_issue_token),
        ("http::csrf::verify_token", builtin_csrf_verify_token),
        ("http::session::sign", builtin_session_sign),
        ("http::session::verify", builtin_session_verify),
    ] {
        globals.push((path, crate::builtins::builtin_pub(path, call)));
    }
}

// -- cookie --------------------------------------------------------------

pub(crate) fn builtin_cookie_parse_header(args: &[Value]) -> RuntimeResult<Value> {
    let header = as_str(args.first().unwrap_or(&Value::Unit)).unwrap_or("");
    let pairs = gossamer_std::http_cookie::parse_cookie_header(header);
    let items: Vec<Value> = pairs
        .into_iter()
        .map(|(name, value)| {
            Value::Tuple(Arc::new(vec![
                Value::String(name.into()),
                Value::String(value.into()),
            ]))
        })
        .collect();
    Ok(Value::Array(Arc::new(items)))
}

pub(crate) fn builtin_cookie_serialize(args: &[Value]) -> RuntimeResult<Value> {
    let name = as_str(args.first().unwrap_or(&Value::Unit)).unwrap_or("");
    let value = as_str(args.get(1).unwrap_or(&Value::Unit)).unwrap_or("");
    let header = gossamer_std::http_cookie::Cookie::new(name, value).to_header_value();
    Ok(Value::String(header.into()))
}

// -- csrf ----------------------------------------------------------------

pub(crate) fn builtin_csrf_issue_token(args: &[Value]) -> RuntimeResult<Value> {
    let key = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::http_csrf::issue_token(&key) {
        Ok(token) => Ok(ok_variant(Value::String(token.into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_csrf_verify_token(args: &[Value]) -> RuntimeResult<Value> {
    let cookie_token = as_str(args.first().unwrap_or(&Value::Unit)).unwrap_or("");
    let supplied_token = as_str(args.get(1).unwrap_or(&Value::Unit)).unwrap_or("");
    let key = bytes_from_value(args.get(2).unwrap_or(&Value::Unit));
    match gossamer_std::http_csrf::verify_token(cookie_token, supplied_token, &key) {
        Ok(()) => Ok(ok_variant(Value::Unit)),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// -- session (SignedCookieStore, SignedOnly mode) ------------------------

pub(crate) fn builtin_session_sign(args: &[Value]) -> RuntimeResult<Value> {
    let payload = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    let key = bytes_from_value(args.get(1).unwrap_or(&Value::Unit));
    let b64_payload = b64url_encode(&payload);
    let mac = gossamer_std::crypto::hmac::sha256_mac(&key, b64_payload.as_bytes());
    let wire = format!("{b64_payload}.{}", b64url_encode(&mac));
    Ok(Value::String(wire.into()))
}

pub(crate) fn builtin_session_verify(args: &[Value]) -> RuntimeResult<Value> {
    let cookie = as_str(args.first().unwrap_or(&Value::Unit)).unwrap_or("");
    let key = bytes_from_value(args.get(1).unwrap_or(&Value::Unit));
    let Some((left, right)) = cookie.split_once('.') else {
        return Ok(err_variant("session: missing separator"));
    };
    let Some(sig) = b64url_decode(right) else {
        return Ok(err_variant("session: signature decode"));
    };
    let mac = gossamer_std::crypto::hmac::sha256_mac(&key, left.as_bytes());
    if !constant_time_eq(&mac, &sig) {
        return Ok(err_variant("session: bad signature"));
    }
    let Some(payload) = b64url_decode(left) else {
        return Ok(err_variant("session: payload decode"));
    };
    match String::from_utf8(payload) {
        Ok(s) => Ok(ok_variant(Value::String(s.into()))),
        Err(_) => Ok(err_variant("session: payload not utf-8")),
    }
}

// -- base64url (RFC 4648 §5, no padding) — matches the runtime shim ------

const B64URL_ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64url_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let n =
            (u32::from(input[i]) << 16) | (u32::from(input[i + 1]) << 8) | u32::from(input[i + 2]);
        out.push(B64URL_ALPHA[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64URL_ALPHA[((n >> 12) & 0x3f) as usize] as char);
        out.push(B64URL_ALPHA[((n >> 6) & 0x3f) as usize] as char);
        out.push(B64URL_ALPHA[(n & 0x3f) as usize] as char);
        i += 3;
    }
    let remaining = input.len() - i;
    if remaining == 1 {
        let n = u32::from(input[i]) << 16;
        out.push(B64URL_ALPHA[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64URL_ALPHA[((n >> 12) & 0x3f) as usize] as char);
    } else if remaining == 2 {
        let n = (u32::from(input[i]) << 16) | (u32::from(input[i + 1]) << 8);
        out.push(B64URL_ALPHA[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64URL_ALPHA[((n >> 12) & 0x3f) as usize] as char);
        out.push(B64URL_ALPHA[((n >> 6) & 0x3f) as usize] as char);
    }
    out
}

fn decode_b64url_char(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'-' => Some(62),
        b'_' => Some(63),
        _ => None,
    }
}

fn b64url_decode(input: &str) -> Option<Vec<u8>> {
    let trimmed = input.trim_end_matches('=');
    let bytes = trimmed.as_bytes();
    let n = bytes.len();
    let out_len = match n % 4 {
        0 => n / 4 * 3,
        2 => n / 4 * 3 + 1,
        3 => n / 4 * 3 + 2,
        _ => return None,
    };
    let mut out = Vec::with_capacity(out_len);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &b in bytes {
        let v = decode_b64url_char(b)?;
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    if bits > 0 && (acc & ((1u32 << bits) - 1)) != 0 {
        return None;
    }
    Some(out)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    gossamer_std::crypto::subtle::constant_time_eq(a, b)
}
