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

pub(crate) fn install_result(globals: &mut Vec<(&'static str, Value)>) {
    let static_entries: &[(&str, BuiltinFnPub)] = &[
        ("is_ok", builtin_result_is_ok),
        ("is_err", builtin_result_is_err),
        ("ok", builtin_result_ok),
        ("err", builtin_result_err),
        ("default", builtin_result_default),
    ];
    for (short, call) in static_entries {
        let qualified: &'static str = Box::leak(format!("result::{short}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, *call)));
    }

    let native_entries: &[(&str, NativeCall)] = &[
        ("map", native_result_map),
        ("map_err", native_result_map_err),
        ("and_then", native_result_and_then),
        ("or_else", native_result_or_else),
        ("default_with", native_result_default_with),
    ];
    for (short, call) in native_entries {
        let qualified: &'static str = Box::leak(format!("result::{short}").into_boxed_str());
        globals.push((qualified, Value::native(qualified, *call)));
    }
}

pub(crate) fn is_ok_variant(v: &Value) -> bool {
    matches!(v, Value::Variant(inner) if inner.name == "Ok")
}

pub(crate) fn is_err_variant(v: &Value) -> bool {
    matches!(v, Value::Variant(inner) if inner.name == "Err")
}

pub(crate) fn ok_payload(v: &Value) -> Option<Value> {
    if let Value::Variant(inner) = v
        && inner.name == "Ok"
        && let Some(first) = inner.fields.first()
    {
        return Some(first.clone());
    }
    None
}

pub(crate) fn err_payload(v: &Value) -> Option<Value> {
    if let Value::Variant(inner) = v
        && inner.name == "Err"
        && let Some(first) = inner.fields.first()
    {
        return Some(first.clone());
    }
    None
}

pub(crate) fn builtin_result_is_ok(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(is_ok_variant(
        args.first().unwrap_or(&Value::Unit),
    )))
}

pub(crate) fn builtin_result_is_err(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(is_err_variant(
        args.first().unwrap_or(&Value::Unit),
    )))
}

pub(crate) fn builtin_result_ok(args: &[Value]) -> RuntimeResult<Value> {
    let r = args.first().unwrap_or(&Value::Unit);
    Ok(ok_payload(r).map_or_else(none_variant, some_variant))
}

pub(crate) fn builtin_result_err(args: &[Value]) -> RuntimeResult<Value> {
    let r = args.first().unwrap_or(&Value::Unit);
    Ok(err_payload(r).map_or_else(none_variant, some_variant))
}

pub(crate) fn builtin_result_default(args: &[Value]) -> RuntimeResult<Value> {
    let fallback = args.first().cloned().unwrap_or(Value::Unit);
    let r = args.get(1).unwrap_or(&Value::Unit);
    Ok(ok_payload(r).unwrap_or(fallback))
}

pub(crate) fn native_result_map(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let f = args.first().cloned().unwrap_or(Value::Unit);
    let r = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| ok_variant(Value::Unit));
    match ok_payload(&r) {
        Some(x) => Ok(ok_variant(dispatch.call_value(&f, vec![x])?)),
        None => Ok(r),
    }
}

pub(crate) fn native_result_map_err(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let f = args.first().cloned().unwrap_or(Value::Unit);
    let r = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| ok_variant(Value::Unit));
    if let Some(e) = err_payload(&r) {
        let mapped = dispatch.call_value(&f, vec![e])?;
        return Ok(Value::variant("Err", vec![mapped]));
    }
    Ok(r)
}

pub(crate) fn native_result_and_then(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let f = args.first().cloned().unwrap_or(Value::Unit);
    let r = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| ok_variant(Value::Unit));
    match ok_payload(&r) {
        Some(x) => dispatch.call_value(&f, vec![x]),
        None => Ok(r),
    }
}

pub(crate) fn native_result_or_else(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let f = args.first().cloned().unwrap_or(Value::Unit);
    let r = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| ok_variant(Value::Unit));
    if let Some(e) = err_payload(&r) {
        return dispatch.call_value(&f, vec![e]);
    }
    Ok(r)
}

pub(crate) fn native_result_default_with(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let f = args.first().cloned().unwrap_or(Value::Unit);
    let r = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| ok_variant(Value::Unit));
    if let Some(x) = ok_payload(&r) {
        return Ok(x);
    }
    let e = err_payload(&r).unwrap_or(Value::Unit);
    dispatch.call_value(&f, vec![e])
}

// ----------------------------------------------------------------------
// crypto (sha256, hmac, rand — always enabled in this crate)
