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

pub(crate) fn install_encoding_binary(globals: &mut Vec<(&'static str, Value)>) {
    use gossamer_std::encoding::binary as bin;

    install_module_pub(
        "encoding::binary",
        &[
            ("put_u16_be", builtin_bin_put_u16_be),
            ("put_u32_be", builtin_bin_put_u32_be),
            ("get_u16_be", builtin_bin_get_u16_be),
            ("get_u16_le", builtin_bin_get_u16_le),
            ("get_u32_be", builtin_bin_get_u32_be),
            ("get_u32_le", builtin_bin_get_u32_le),
            ("put_u16_le", builtin_bin_put_u16_le),
            ("put_u32_le", builtin_bin_put_u32_le),
            ("get_u64_be", builtin_bin_get_u64_be),
            ("put_u64_be", builtin_bin_put_u64_be),
            ("get_u64_le", builtin_bin_get_u64_le),
            ("put_u64_le", builtin_bin_put_u64_le),
            ("uvarint", builtin_bin_uvarint),
            ("varint", builtin_bin_varint),
        ],
        globals,
    );

    // Register bare names too for backward compat.
    for (name, f) in &[
        ("put_u16_be", builtin_bin_put_u16_be as BuiltinFnPub),
        ("put_u32_be", builtin_bin_put_u32_be as BuiltinFnPub),
    ] {
        globals.push((*name, crate::builtins::builtin_pub(name, *f)));
    }

    // Suppress unused warning - the module `bin` is only used for its
    // associated functions, which we call through the function pointers below.
    let _ = bin::get_u8;
}

pub(crate) fn bytes_from_value(v: &Value) -> Vec<u8> {
    match v {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|elem| match elem {
                Value::Int(n) => u8::try_from(*n).ok(),
                _ => None,
            })
            .collect(),
        // Fast-path for typed integer arrays produced by literal [n, ...] with i64 elements.
        Value::IntArray(arr) => arr.iter().filter_map(|&n| u8::try_from(n).ok()).collect(),
        Value::String(s) => s.as_str().as_bytes().to_vec(),
        _ => Vec::new(),
    }
}

pub(crate) fn builtin_bin_put_u16_be(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u16;
    let mut buf = [0u8; 2];
    gossamer_std::encoding::binary::put_u16_be(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf.iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
pub(crate) fn builtin_bin_put_u32_be(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u32;
    let mut buf = [0u8; 4];
    gossamer_std::encoding::binary::put_u32_be(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf.iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
pub(crate) fn builtin_bin_put_u16_le(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u16;
    let mut buf = [0u8; 2];
    gossamer_std::encoding::binary::put_u16_le(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf.iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
pub(crate) fn builtin_bin_put_u32_le(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u32;
    let mut buf = [0u8; 4];
    gossamer_std::encoding::binary::put_u32_le(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf.iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
pub(crate) fn builtin_bin_put_u64_be(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u64;
    let mut buf = [0u8; 8];
    gossamer_std::encoding::binary::put_u64_be(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf.iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
pub(crate) fn builtin_bin_put_u64_le(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u64;
    let mut buf = [0u8; 8];
    gossamer_std::encoding::binary::put_u64_le(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf.iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
pub(crate) fn builtin_bin_get_u16_be(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.len() < 2 {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(i64::from(
        gossamer_std::encoding::binary::get_u16_be(&bytes),
    ))))
}
pub(crate) fn builtin_bin_get_u16_le(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.len() < 2 {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(i64::from(
        gossamer_std::encoding::binary::get_u16_le(&bytes),
    ))))
}
pub(crate) fn builtin_bin_get_u32_be(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.len() < 4 {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(i64::from(
        gossamer_std::encoding::binary::get_u32_be(&bytes),
    ))))
}
pub(crate) fn builtin_bin_get_u32_le(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.len() < 4 {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(i64::from(
        gossamer_std::encoding::binary::get_u32_le(&bytes),
    ))))
}
pub(crate) fn builtin_bin_get_u64_be(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.len() < 8 {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(
        gossamer_std::encoding::binary::get_u64_be(&bytes) as i64,
    )))
}
pub(crate) fn builtin_bin_get_u64_le(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.len() < 8 {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(
        gossamer_std::encoding::binary::get_u64_le(&bytes) as i64,
    )))
}
pub(crate) fn builtin_bin_uvarint(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::encoding::binary::uvarint(&bytes) {
        Ok((v, n)) => Ok(ok_variant(Value::Tuple(Arc::new(vec![
            Value::Int(v as i64),
            Value::Int(n as i64),
        ])))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}
pub(crate) fn builtin_bin_varint(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::encoding::binary::varint(&bytes) {
        Ok((v, n)) => Ok(ok_variant(Value::Tuple(Arc::new(vec![
            Value::Int(v),
            Value::Int(n as i64),
        ])))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// encoding::csv
