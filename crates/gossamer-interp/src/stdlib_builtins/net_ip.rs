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
use std::sync::atomic::{AtomicBool as StdAtomicBool, AtomicI64 as StdAtomicI64, Ordering};

use crate::value::SmolStr;

use gossamer_std::bufio as bufio_std;
use gossamer_std::math as math_std;
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

pub(crate) fn install_net_ip(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        ("parse", builtin_net_ip_parse as BuiltinFnPub),
        ("is_valid", builtin_net_ip_is_valid),
        ("is_v4", builtin_net_ip_is_v4),
        ("is_v6", builtin_net_ip_is_v6),
        ("to_string", builtin_net_ip_to_string),
        ("is_loopback", builtin_net_ip_is_loopback),
        ("is_private", builtin_net_ip_is_private),
        ("is_multicast", builtin_net_ip_is_multicast),
        ("is_unspecified", builtin_net_ip_is_unspecified),
        ("octets", builtin_net_ip_octets),
    ] {
        let q: &'static str = Box::leak(format!("net::ip::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

pub(crate) fn ip_to_value(ip: gossamer_std::net::ip::Ip) -> Value {
    let (tag, octets): (&str, Vec<u8>) = match &ip {
        gossamer_std::net::ip::Ip::V4(_) => ("v4", ip.octets()),
        gossamer_std::net::ip::Ip::V6(_) => ("v6", ip.octets()),
    };
    Value::struct_(
        "net::ip::Ip",
        Arc::unwrap_or_clone(Arc::new(vec![
            (Ident::new("__tag"), Value::String(tag.into())),
            (Ident::new("__str"), Value::String(ip.to_string().into())),
            (Ident::new("__octets"), bytes_to_value_array(&octets)),
        ])),
    )
}

pub(crate) fn ip_from_value(v: &Value) -> Option<gossamer_std::net::ip::Ip> {
    let s = match v {
        Value::Struct(inner) if inner.name == "net::ip::Ip" => {
            inner.fields.iter().find_map(|(i, val)| {
                if i.name == "__str" {
                    if let Value::String(s) = val {
                        Some(s.as_str().to_string())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
        }
        Value::String(s) => Some(s.as_str().to_string()),
        _ => None,
    }?;
    gossamer_std::net::ip::parse(&s).ok()
}

pub(crate) fn builtin_net_ip_parse(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("").to_string();
    match gossamer_std::net::ip::parse(&s) {
        Ok(ip) => Ok(ok_variant(ip_to_value(ip))),
        Err(e) => Ok(err_variant(e)),
    }
}

pub(crate) fn builtin_net_ip_is_valid(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Bool(gossamer_std::net::ip::is_valid(s)))
}

pub(crate) fn builtin_net_ip_is_v4(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Bool(gossamer_std::net::ip::is_v4(s)))
}

pub(crate) fn builtin_net_ip_is_v6(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Bool(gossamer_std::net::ip::is_v6(s)))
}

pub(crate) fn builtin_net_ip_to_string(args: &[Value]) -> RuntimeResult<Value> {
    match ip_from_value(args.first().unwrap_or(&Value::Unit)) {
        Some(ip) => Ok(Value::String(ip.to_string().into())),
        None => Ok(Value::String("".into())),
    }
}

pub(crate) fn builtin_net_ip_is_loopback(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(
        ip_from_value(args.first().unwrap_or(&Value::Unit)).is_some_and(|ip| ip.is_loopback()),
    ))
}

pub(crate) fn builtin_net_ip_is_private(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(
        ip_from_value(args.first().unwrap_or(&Value::Unit)).is_some_and(|ip| ip.is_private()),
    ))
}

pub(crate) fn builtin_net_ip_is_multicast(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(
        ip_from_value(args.first().unwrap_or(&Value::Unit)).is_some_and(|ip| ip.is_multicast()),
    ))
}

pub(crate) fn builtin_net_ip_is_unspecified(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(
        ip_from_value(args.first().unwrap_or(&Value::Unit)).is_some_and(|ip| ip.is_unspecified()),
    ))
}

pub(crate) fn builtin_net_ip_octets(args: &[Value]) -> RuntimeResult<Value> {
    match ip_from_value(args.first().unwrap_or(&Value::Unit)) {
        Some(ip) => Ok(bytes_to_value_array(&ip.octets())),
        None => Ok(Value::Array(Arc::new(vec![]))),
    }
}

// ----------------------------------------------------------------------
// thread builtins
