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

pub(crate) fn install_os_extras(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "os",
        &[
            ("set_env", builtin_os_set_env),
            ("unset_env", builtin_os_unset_env),
            ("is_file", builtin_os_is_file),
            ("is_dir", builtin_os_is_dir),
            ("is_symlink", builtin_os_is_symlink),
            ("file_size", builtin_os_file_size),
            ("home", builtin_os_home),
            ("temp_dir", builtin_os_temp_dir),
            ("set_cwd", builtin_os_set_cwd),
            ("canonicalize", builtin_os_canonicalize),
            ("remove_dir", builtin_os_remove_dir),
            ("remove_dir_all", builtin_os_remove_dir_all),
            ("copy", builtin_os_copy),
        ],
        globals,
    );
}

pub(crate) fn builtin_os_set_env(args: &[Value]) -> RuntimeResult<Value> {
    let name = match arg_str_at(args, 0, "os::set_env", "name") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    let value = args.get(1).and_then(as_str).unwrap_or("").to_string();
    match os_std::set_env(&name, &value) {
        Ok(()) => Ok(ok_variant(Value::Unit)),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_os_unset_env(args: &[Value]) -> RuntimeResult<Value> {
    let name = args.first().and_then(as_str).unwrap_or("");
    os_std::unset_env(name);
    Ok(Value::Unit)
}

pub(crate) fn builtin_os_is_file(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Bool(os_std::is_file(path)))
}

pub(crate) fn builtin_os_is_dir(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Bool(os_std::is_dir(path)))
}

pub(crate) fn builtin_os_is_symlink(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Bool(os_std::is_symlink(path)))
}

pub(crate) fn builtin_os_file_size(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Int(
        i64::try_from(os_std::file_size(path)).unwrap_or(i64::MAX),
    ))
}

pub(crate) fn builtin_os_home(_args: &[Value]) -> RuntimeResult<Value> {
    match os_std::home() {
        Some(s) => Ok(some_variant(Value::String(s.into()))),
        None => Ok(none_variant()),
    }
}

pub(crate) fn builtin_os_temp_dir(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(os_std::temp_dir().into()))
}

pub(crate) fn builtin_os_set_cwd(args: &[Value]) -> RuntimeResult<Value> {
    let path = match arg_str_at(args, 0, "os::set_cwd", "path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match os_std::set_cwd(&path) {
        Ok(()) => Ok(ok_variant(Value::Unit)),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_os_canonicalize(args: &[Value]) -> RuntimeResult<Value> {
    let path = match arg_str_at(args, 0, "os::canonicalize", "path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match os_std::canonicalize(&path) {
        Ok(s) => Ok(ok_variant(Value::String(s.into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_os_remove_dir(args: &[Value]) -> RuntimeResult<Value> {
    let path = match arg_str_at(args, 0, "os::remove_dir", "path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match os_std::remove_dir(&path) {
        Ok(()) => Ok(ok_variant(Value::Unit)),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_os_remove_dir_all(args: &[Value]) -> RuntimeResult<Value> {
    let path = match arg_str_at(args, 0, "os::remove_dir_all", "path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match os_std::remove_dir_all(&path) {
        Ok(()) => Ok(ok_variant(Value::Unit)),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_os_copy(args: &[Value]) -> RuntimeResult<Value> {
    let src = match arg_str_at(args, 0, "os::copy", "source path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    let dst = match arg_str_at(args, 1, "os::copy", "destination path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match os_std::copy(&src, &dst) {
        Ok(n) => Ok(ok_variant(Value::Int(i64::try_from(n).unwrap_or(i64::MAX)))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// fs extras
