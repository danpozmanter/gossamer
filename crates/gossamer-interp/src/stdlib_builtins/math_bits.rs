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

pub(crate) fn install_math_bits(globals: &mut Vec<(&'static str, Value)>) {
    // Register only qualified names; bare `len`, `add`, `sub`, `mul`, `div`
    // would shadow built-in array/string methods and arithmetic operators.
    let entries: &[(&str, BuiltinFnPub)] = &[
        ("count_ones", builtin_bits_count_ones),
        ("count_zeros", builtin_bits_count_zeros),
        ("leading_zeros", builtin_bits_leading_zeros),
        ("trailing_zeros", builtin_bits_trailing_zeros),
        ("rotate_left", builtin_bits_rotate_left),
        ("rotate_right", builtin_bits_rotate_right),
        ("reverse_bits", builtin_bits_reverse_bits),
        ("reverse_bytes", builtin_bits_reverse_bytes),
        ("len", builtin_bits_len),
        ("add", builtin_bits_add),
        ("sub", builtin_bits_sub),
        ("mul", builtin_bits_mul),
        ("div", builtin_bits_div),
    ];
    for (short, call) in entries {
        let qualified: &'static str = Box::leak(format!("math::bits::{short}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, *call)));
    }
}

pub(crate) fn builtin_bits_count_ones(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(i64::from(math_std::bits::count_ones(arg_u64(
        args, 0,
    )))))
}
pub(crate) fn builtin_bits_count_zeros(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(i64::from(math_std::bits::count_zeros(arg_u64(
        args, 0,
    )))))
}
pub(crate) fn builtin_bits_leading_zeros(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(i64::from(math_std::bits::leading_zeros(
        arg_u64(args, 0),
    ))))
}
pub(crate) fn builtin_bits_trailing_zeros(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(i64::from(math_std::bits::trailing_zeros(
        arg_u64(args, 0),
    ))))
}
pub(crate) fn builtin_bits_rotate_left(args: &[Value]) -> RuntimeResult<Value> {
    let x = arg_u64(args, 0);
    let n = args.get(1).and_then(value_to_int).unwrap_or(0);
    Ok(Value::Int(math_std::bits::rotate_left(x, n) as i64))
}
pub(crate) fn builtin_bits_rotate_right(args: &[Value]) -> RuntimeResult<Value> {
    let x = arg_u64(args, 0);
    let n = args.get(1).and_then(value_to_int).unwrap_or(0);
    Ok(Value::Int(math_std::bits::rotate_right(x, n) as i64))
}
pub(crate) fn builtin_bits_reverse_bits(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(
        math_std::bits::reverse_bits(arg_u64(args, 0)) as i64
    ))
}
pub(crate) fn builtin_bits_reverse_bytes(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(
        math_std::bits::reverse_bytes(arg_u64(args, 0)) as i64
    ))
}
pub(crate) fn builtin_bits_len(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(i64::from(math_std::bits::len(arg_u64(args, 0)))))
}
pub(crate) fn builtin_bits_add(args: &[Value]) -> RuntimeResult<Value> {
    let (sum, carry) = math_std::bits::add(arg_u64(args, 0), arg_u64(args, 1), arg_u64(args, 2));
    Ok(Value::Tuple(Arc::from(vec![
        Value::Int(sum as i64),
        Value::Int(carry as i64),
    ])))
}
pub(crate) fn builtin_bits_sub(args: &[Value]) -> RuntimeResult<Value> {
    let (diff, borrow) = math_std::bits::sub(arg_u64(args, 0), arg_u64(args, 1), arg_u64(args, 2));
    Ok(Value::Tuple(Arc::from(vec![
        Value::Int(diff as i64),
        Value::Int(borrow as i64),
    ])))
}
pub(crate) fn builtin_bits_mul(args: &[Value]) -> RuntimeResult<Value> {
    let (hi, lo) = math_std::bits::mul(arg_u64(args, 0), arg_u64(args, 1));
    Ok(Value::Tuple(Arc::from(vec![
        Value::Int(hi as i64),
        Value::Int(lo as i64),
    ])))
}
pub(crate) fn builtin_bits_div(args: &[Value]) -> RuntimeResult<Value> {
    let y = arg_u64(args, 2);
    if y == 0 {
        return Ok(err_variant("math::bits::div: division by zero".to_string()));
    }
    let (q, r) = math_std::bits::div(arg_u64(args, 0), arg_u64(args, 1), y);
    Ok(Value::Tuple(Arc::from(vec![
        Value::Int(q as i64),
        Value::Int(r as i64),
    ])))
}

// ----------------------------------------------------------------------
// unicode

pub(crate) fn arg_char(args: &[Value], idx: usize) -> char {
    match args.get(idx) {
        Some(Value::Char(c)) => *c,
        Some(Value::String(s)) => s.as_str().chars().next().unwrap_or('\0'),
        Some(Value::Int(n)) => char::from_u32(*n as u32).unwrap_or('\0'),
        _ => '\0',
    }
}
