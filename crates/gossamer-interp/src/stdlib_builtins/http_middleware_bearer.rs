#![allow(
    unused_imports,
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::wildcard_imports,
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    clippy::module_name_repetitions
)]
//! Bytecode-VM `http::middleware::bearer_ok` - the interp mirror of
//! `gos_rt_http_bearer_ok`. A `native` builtin, because it invokes the
//! caller's Gossamer `verify` closure through the interpreter
//! dispatcher (a plain `BuiltinFnPub` cannot call a closure). Same
//! registration shape as `sync::RwLock::with_read` / `sync::Once::call`.
//!
//! Extracts the `Authorization: Bearer <token>` token and runs
//! `verify(token)`, returning its bool; a missing or non-`Bearer`
//! header returns `false` without calling `verify`. The 401-vs-body
//! response shaping is left to Gossamer handler code, so it is
//! bit-identical across tiers by construction.

use crate::value::{NativeDispatch, RuntimeResult, SmolStr, Value};

use super::*;

pub(crate) fn install_http_middleware_bearer(globals: &mut Vec<(&'static str, Value)>) {
    for name in ["middleware::bearer_ok", "http::middleware::bearer_ok"] {
        globals.push((name, Value::native(name, native_bearer_ok)));
    }
}

/// Case-insensitively reads a header value from a Request value's
/// `headers` field (a `[(String, String)]` array of tuples).
fn request_header(req: &Value, name: &str) -> Option<String> {
    let Value::Struct(inner) = req else {
        return None;
    };
    for (field, val) in &inner.fields {
        if (*field) == "headers" {
            if let Value::Array(items) = val {
                for item in items.iter() {
                    if let Value::Tuple(t) = item {
                        if let (Some(Value::String(k)), Some(Value::String(v))) =
                            (t.first(), t.get(1))
                        {
                            if k.as_str().eq_ignore_ascii_case(name) {
                                return Some(v.as_str().to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Bearer token from an `Authorization` header value, or `None` when
/// the scheme is absent or not `Bearer` (case-insensitive). Mirrors
/// the compiled `bearer_token` so both tiers split the header
/// identically.
fn bearer_token(auth: &str) -> Option<String> {
    let (scheme, rest) = auth.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") {
        Some(rest.trim().to_string())
    } else {
        None
    }
}

/// `middleware::bearer_ok(req, verify) -> bool`. Returns whether the
/// request carries a `Bearer` token the `verify` closure accepts;
/// `false` (without calling `verify`) when no bearer header is present.
pub(crate) fn native_bearer_ok(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let Some(req) = args.first() else {
        return Ok(Value::Bool(false));
    };
    let Some(auth) = request_header(req, "authorization") else {
        return Ok(Value::Bool(false));
    };
    let Some(token) = bearer_token(&auth) else {
        return Ok(Value::Bool(false));
    };
    let Some(verify) = args.get(1).cloned() else {
        return Ok(Value::Bool(false));
    };
    let result = dispatch.call_value(&verify, vec![Value::String(SmolStr::from(token))])?;
    Ok(Value::Bool(matches!(result, Value::Bool(true))))
}
