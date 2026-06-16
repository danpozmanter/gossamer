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

pub(crate) fn install_compress(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        ("gzip::encode", builtin_compress_gzip_encode as BuiltinFnPub),
        ("gzip::decode", builtin_compress_gzip_decode),
        ("flate::compress", builtin_compress_flate_compress),
        ("flate::decompress", builtin_compress_flate_decompress),
        ("zlib::compress", builtin_compress_zlib_compress),
        ("zlib::decompress", builtin_compress_zlib_decompress),
        ("zstd::encode", builtin_compress_zstd_encode),
        ("zstd::encode_level", builtin_compress_zstd_encode_level),
        ("zstd::decode", builtin_compress_zstd_decode),
    ] {
        let q: &'static str = Box::leak(format!("compress::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

pub(crate) fn builtin_compress_gzip_encode(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    let level = args.get(1).and_then(value_to_int).unwrap_or(6) as u32;
    let lvl = gossamer_std::compress::gzip::Level::new(level.clamp(0, 9))
        .unwrap_or(gossamer_std::compress::gzip::Level::DEFAULT);
    match gossamer_std::compress::gzip::encode(&input, lvl) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_compress_gzip_decode(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::compress::gzip::decode(&input) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_compress_zstd_encode(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::compress::zstd::encode(&input) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_compress_zstd_encode_level(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    let level = args.get(1).and_then(value_to_int).unwrap_or(3) as i32;
    match gossamer_std::compress::zstd::encode_level(&input, level) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_compress_zstd_decode(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::compress::zstd::decode(&input) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_compress_flate_compress(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    let level = args.get(1).and_then(value_to_int).unwrap_or(6) as u32;
    match gossamer_std::compress::flate::compress(&input, level) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_compress_flate_decompress(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::compress::flate::decompress(&input) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_compress_zlib_compress(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    let level = args.get(1).and_then(value_to_int).unwrap_or(6) as u32;
    match gossamer_std::compress::zlib::compress(&input, level) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_compress_zlib_decompress(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::compress::zlib::decompress(&input) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// hash::fnv
