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

pub(crate) fn install_option(globals: &mut Vec<(&'static str, Value)>) {
    let static_entries: &[(&str, BuiltinFnPub)] = &[
        ("is_some", builtin_option_is_some),
        ("is_none", builtin_option_is_none),
        ("default", builtin_option_default),
        ("or", builtin_option_or),
        ("flatten", builtin_option_flatten),
        ("zip", builtin_option_zip),
    ];
    for (short, call) in static_entries {
        let qualified: &'static str = Box::leak(format!("option::{short}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, *call)));
    }

    let native_entries: &[(&str, NativeCall)] = &[
        ("map", native_option_map),
        ("and_then", native_option_and_then),
        ("filter", native_option_filter),
        ("default_with", native_option_default_with),
        ("or_else", native_option_or_else),
        ("iter", native_option_iter),
    ];
    for (short, call) in native_entries {
        let qualified: &'static str = Box::leak(format!("option::{short}").into_boxed_str());
        globals.push((qualified, Value::native(qualified, *call)));
    }
}

pub(crate) fn is_some_variant(v: &Value) -> bool {
    matches!(v, Value::Variant(inner) if inner.name == "Some")
}

pub(crate) fn is_none_variant(v: &Value) -> bool {
    matches!(v, Value::Variant(inner) if inner.name == "None")
}

pub(crate) fn builtin_option_is_some(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(is_some_variant(
        args.first().unwrap_or(&Value::Unit),
    )))
}

pub(crate) fn builtin_option_is_none(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(is_none_variant(
        args.first().unwrap_or(&Value::Unit),
    )))
}

pub(crate) fn builtin_option_default(args: &[Value]) -> RuntimeResult<Value> {
    let fallback = args.first().cloned().unwrap_or(Value::Unit);
    let opt = args.get(1).unwrap_or(&Value::Unit);
    Ok(some_payload(opt).unwrap_or(fallback))
}

pub(crate) fn builtin_option_or(args: &[Value]) -> RuntimeResult<Value> {
    let alt = args.first().cloned().unwrap_or_else(none_variant);
    let opt = args.get(1).cloned().unwrap_or_else(none_variant);
    Ok(if is_some_variant(&opt) { opt } else { alt })
}

pub(crate) fn builtin_option_flatten(args: &[Value]) -> RuntimeResult<Value> {
    let outer = args.first().cloned().unwrap_or_else(none_variant);
    if let Some(inner) = some_payload(&outer) {
        return Ok(inner);
    }
    Ok(none_variant())
}

pub(crate) fn builtin_option_zip(args: &[Value]) -> RuntimeResult<Value> {
    let a = args.first().cloned().unwrap_or_else(none_variant);
    let b = args.get(1).cloned().unwrap_or_else(none_variant);
    match (some_payload(&a), some_payload(&b)) {
        (Some(x), Some(y)) => Ok(some_variant(Value::Tuple(Arc::new(vec![x, y])))),
        _ => Ok(none_variant()),
    }
}

pub(crate) fn native_option_map(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let f = args.first().cloned().unwrap_or(Value::Unit);
    let opt = args.get(1).cloned().unwrap_or_else(none_variant);
    match some_payload(&opt) {
        Some(x) => Ok(some_variant(dispatch.call_value(&f, vec![x])?)),
        None => Ok(none_variant()),
    }
}

pub(crate) fn native_option_and_then(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let f = args.first().cloned().unwrap_or(Value::Unit);
    let opt = args.get(1).cloned().unwrap_or_else(none_variant);
    match some_payload(&opt) {
        Some(x) => dispatch.call_value(&f, vec![x]),
        None => Ok(none_variant()),
    }
}

pub(crate) fn native_option_filter(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let p = args.first().cloned().unwrap_or(Value::Unit);
    let opt = args.get(1).cloned().unwrap_or_else(none_variant);
    if let Some(x) = some_payload(&opt) {
        if matches!(dispatch.call_value(&p, vec![x.clone()])?, Value::Bool(true)) {
            return Ok(some_variant(x));
        }
    }
    Ok(none_variant())
}

pub(crate) fn native_option_default_with(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let f = args.first().cloned().unwrap_or(Value::Unit);
    let opt = args.get(1).unwrap_or(&Value::Unit);
    if let Some(x) = some_payload(opt) {
        return Ok(x);
    }
    dispatch.call_value(&f, Vec::new())
}

pub(crate) fn native_option_or_else(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let f = args.first().cloned().unwrap_or(Value::Unit);
    let opt = args.get(1).cloned().unwrap_or_else(none_variant);
    if is_some_variant(&opt) {
        return Ok(opt);
    }
    dispatch.call_value(&f, Vec::new())
}

pub(crate) fn native_option_iter(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    // Accessor form (`opt |> option::iter -> [T]`): zero- or
    // one-element array, matching the checker's signature row and the
    // compiled tiers' `gos_rt_option_iter`.
    if args.len() == 1 {
        let opt = args.first().unwrap_or(&Value::Unit);
        let elems = some_payload(opt).map_or_else(Vec::new, |x| vec![x]);
        return Ok(Value::Array(Arc::new(elems)));
    }
    // Legacy for-each form (`option::iter(f, opt)`).
    let f = args.first().cloned().unwrap_or(Value::Unit);
    let opt = args.get(1).unwrap_or(&Value::Unit);
    if let Some(x) = some_payload(opt) {
        dispatch.call_value(&f, vec![x])?;
    }
    Ok(Value::Unit)
}

// ----------------------------------------------------------------------
// result - F#-style chaining surface for `Result<T, E>` (SPEC §10.4b).
// Data-last. The `?` operator stays the right tool for short-circuit
// propagation; these are for in-pipeline transformation.
