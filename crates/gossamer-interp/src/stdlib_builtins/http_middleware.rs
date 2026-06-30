#![allow(
    unused_imports,
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::items_after_statements,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::option_if_let_else,
    clippy::match_same_arms,
    clippy::if_not_else,
    clippy::single_match_else,
    clippy::needless_pass_by_value,
    clippy::manual_let_else,
    clippy::redundant_else,
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::map_unwrap_or,
    clippy::struct_excessive_bools,
    clippy::module_name_repetitions,
    clippy::unnecessary_wraps,
    clippy::large_enum_variant,
    clippy::if_same_then_else,
    clippy::single_match,
    clippy::useless_conversion,
    clippy::needless_borrows_for_generic_args,
    clippy::let_and_return,
    unsafe_op_in_unsafe_fn,
    unsafe_code,
    clippy::missing_safety_doc,
    clippy::undocumented_unsafe_blocks,
    clippy::needless_collect,
    clippy::elidable_lifetime_names,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::missing_const_for_fn,
    clippy::needless_range_loop,
    clippy::cognitive_complexity,
    clippy::unused_io_amount,
    clippy::ptr_arg,
    clippy::ptr_as_ptr,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::single_call_fn,
    clippy::unused_self,
    clippy::range_plus_one,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::cast_ptr_alignment,
    clippy::manual_assert,
    clippy::manual_string_new,
    clippy::match_bool,
    clippy::nonminimal_bool,
    clippy::redundant_pattern_matching,
    clippy::useless_let_if_seq
)]
//! Wires up Gossamer-callable builtins for stdlib modules whose
//! Rust-side implementation already exists but had no user-facing
//! exposure. Each `install_*` helper is invoked from
//! `builtins::install` so user code that writes
//! `strings::join`, `strconv::parse_i64`, `net::TcpStream::connect`,
//! `time::Instant::now`, etc. resolves to a real callable.
//!
//! All builtins return a `Result`-shaped variant (`Ok` / `Err`) on
//! fallible operations so callers can chain `?` without wrapping.

use std::cell::RefCell;
use std::collections::HashMap as StdHashMap;
use std::io::Read as IoRead;
use std::sync::Arc;

use gossamer_ast::Ident;
use std::sync::atomic::Ordering;

use crate::value::SmolStr;

use gossamer_std::bufio as bufio_std;
use gossamer_std::math as math_std;
#[cfg(not(target_arch = "wasm32"))]
use gossamer_std::net as net_std;
use gossamer_std::os as os_std;
use gossamer_std::path as path_std;
use gossamer_std::strconv as strconv_std;
use gossamer_std::strings as strings_std;
use gossamer_std::unicode as unicode_std;
use gossamer_std::utf8 as utf8_std;

use gossamer_std::iter as iter_std;
use gossamer_std::utf16 as utf16_std;

use crate::builtins::{
    BuiltinFnPub, as_str, err_variant, install_module_pub, none_variant, ok_variant, some_variant,
    value_to_int,
};
use crate::value::{MapKey, NativeCall, NativeDispatch, RuntimeResult, Value};

/// Entry point invoked from `builtins::install`.
use super::*;

pub(crate) fn install_http_middleware(globals: &mut Vec<(&'static str, Value)>) {
    for (name, call) in [
        ("new_request_id", builtin_mw_new_request_id as BuiltinFnPub),
        ("decode_basic_auth", builtin_mw_decode_basic_auth),
        ("accepts_gzip", builtin_mw_accepts_gzip),
        ("tag", builtin_mw_tag),
    ] {
        let qualified: &'static str =
            Box::leak(format!("http::middleware::{name}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, call)));
        let short: &'static str = Box::leak(format!("middleware::{name}").into_boxed_str());
        globals.push((short, crate::builtins::builtin_pub(short, call)));
    }
}

pub(crate) fn builtin_mw_new_request_id(_args: &[Value]) -> RuntimeResult<Value> {
    use std::sync::atomic::AtomicU64;
    pub(crate) static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    Ok(Value::String(format!("{nanos:x}-{n:x}").into()))
}

/// `http::middleware::decode_basic_auth(header) -> Option<(String, String)>`.
/// Decodes a `Basic <base64(user:pass)>` Authorization header value (the
/// `Basic ` scheme prefix is optional) into `(user, password)`, or `None`
/// for malformed / non-decodable input. Mirrors the compiled-tier
/// `gos_rt_mw_decode_basic_auth` shim so both tiers classify identically.
pub(crate) fn builtin_mw_decode_basic_auth(args: &[Value]) -> RuntimeResult<Value> {
    let header = arg_str(args.first());
    let token = header.strip_prefix("Basic ").unwrap_or(&header);
    let Ok(decoded) = gossamer_std::encoding::base64::decode(token.trim()) else {
        return Ok(none_variant());
    };
    let Ok(decoded) = String::from_utf8(decoded) else {
        return Ok(none_variant());
    };
    let Some((user, pass)) = decoded.split_once(':') else {
        return Ok(none_variant());
    };
    let pair = Value::Tuple(Arc::from(vec![
        Value::String(user.to_string().into()),
        Value::String(pass.to_string().into()),
    ]));
    Ok(some_variant(pair))
}

/// `http::middleware::tag(inner) -> Handler` - wraps a handler into a
/// `Middleware` value carrying the inner handler; `Middleware::serve`
/// runs the inner serve and prepends `mw:` to the response body. The
/// deterministic interp twin of the compiled `gos_rt_middleware_new` /
/// `gos_rt_middleware_serve` composition.
pub(crate) fn builtin_mw_tag(args: &[Value]) -> RuntimeResult<Value> {
    let inner = args.first().cloned().unwrap_or(Value::Unit);
    Ok(Value::struct_("Middleware", vec![("__mw_inner", inner)]))
}

/// Prepends `mw:` to a response body Value, preserving its String /
/// byte-array shape so chained middleware compose byte-identically with
/// the compiled tier.
fn prepend_mw(body: &Value) -> Value {
    match body {
        Value::String(s) => Value::String(SmolStr::from(format!("mw:{}", s.as_str()))),
        Value::Array(items) => {
            let mut bytes: Vec<Value> = b"mw:".iter().map(|b| Value::Int(i64::from(*b))).collect();
            bytes.extend(items.iter().cloned());
            Value::Array(Arc::new(bytes))
        }
        other => other.clone(),
    }
}

/// `Middleware::serve(mw, request)` - invoked by `http::serve`'s dispatch
/// when the handler is a composed `Middleware`. Runs the wrapped handler
/// (a struct handler's `{T}::serve` or a nested `Middleware::serve`) then
/// applies the `mw:` body transform.
pub(crate) fn native_middleware_serve(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let inner = match args.first() {
        Some(Value::Struct(s)) => s
            .fields
            .iter()
            .find(|(f, _)| (*f) == "__mw_inner")
            .map(|(_, v)| v.clone()),
        _ => None,
    };
    let Some(inner) = inner else {
        return Ok(err_variant("middleware: missing inner handler"));
    };
    let request = args.get(1).cloned().unwrap_or(Value::Unit);
    let inner_serve = match &inner {
        Value::Struct(s) => format!("{}::serve", s.name),
        _ => "serve".to_string(),
    };
    let result = dispatch.call_fn(&inner_serve, vec![inner, request])?;
    // The wrapped serve returns `Ok(Response)` or a bare `Response`.
    let response = match &result {
        Value::Variant(v) if v.name == "Ok" && !v.fields.is_empty() => &v.fields[0],
        other => other,
    };
    let Value::Struct(resp_inner) = response else {
        return Ok(result);
    };
    let new_fields: Vec<(&'static str, Value)> = resp_inner
        .fields
        .iter()
        .map(|(f, v)| {
            if *f == "body" {
                (*f, prepend_mw(v))
            } else {
                (*f, v.clone())
            }
        })
        .collect();
    Ok(ok_variant(Value::struct_("Response", new_fields)))
}

pub(crate) fn builtin_mw_accepts_gzip(args: &[Value]) -> RuntimeResult<Value> {
    let header = arg_str(args.first());
    let accepts = header
        .split(',')
        .any(|tok| tok.trim().eq_ignore_ascii_case("gzip"));
    Ok(Value::Bool(accepts))
}

// ----------------------------------------------------------------------
// uuid
