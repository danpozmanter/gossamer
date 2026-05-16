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

pub(crate) fn install_math_big(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        // signed Int
        ("int_from_str", builtin_big_int_from_str as BuiltinFnPub),
        ("int_from_i64", builtin_big_int_from_i64),
        ("int_to_str", builtin_big_int_to_str),
        ("int_to_hex", builtin_big_int_to_hex),
        ("int_to_i64", builtin_big_int_to_i64),
        ("int_is_zero", builtin_big_int_is_zero),
        ("int_is_positive", builtin_big_int_is_positive),
        ("int_is_negative", builtin_big_int_is_negative),
        ("int_add", builtin_big_int_add),
        ("int_sub", builtin_big_int_sub),
        ("int_mul", builtin_big_int_mul),
        ("int_div", builtin_big_int_div),
        ("int_rem", builtin_big_int_rem),
        ("int_pow", builtin_big_int_pow),
        ("int_abs", builtin_big_int_abs),
        ("int_neg", builtin_big_int_neg),
        ("int_gcd", builtin_big_int_gcd),
        ("int_lcm", builtin_big_int_lcm),
        ("int_cmp", builtin_big_int_cmp),
        // unsigned Uint
        ("uint_from_str", builtin_big_uint_from_str),
        ("uint_from_u64", builtin_big_uint_from_u64),
        ("uint_to_str", builtin_big_uint_to_str),
        ("uint_to_hex", builtin_big_uint_to_hex),
        ("uint_to_u64", builtin_big_uint_to_u64),
        ("uint_is_zero", builtin_big_uint_is_zero),
        ("uint_add", builtin_big_uint_add),
        ("uint_mul", builtin_big_uint_mul),
        ("uint_pow", builtin_big_uint_pow),
        ("uint_pow_mod", builtin_big_uint_pow_mod),
        ("uint_bit_len", builtin_big_uint_bit_len),
        // free functions
        ("factorial", builtin_big_factorial),
    ] {
        let q: &'static str = Box::leak(format!("math::big::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

pub(crate) fn big_int_from_value(v: &Value) -> gossamer_std::math::big::Int {
    let s = match v {
        Value::String(s) => s.as_str().to_string(),
        Value::Int(n) => n.to_string(),
        _ => "0".to_string(),
    };
    gossamer_std::math::big::Int::parse(&s)
        .unwrap_or_else(|_| gossamer_std::math::big::Int::from_i64(0))
}

pub(crate) fn big_uint_from_value(v: &Value) -> gossamer_std::math::big::Uint {
    let s = match v {
        Value::String(s) => s.as_str().to_string(),
        Value::Int(n) => n.abs().to_string(),
        _ => "0".to_string(),
    };
    gossamer_std::math::big::Uint::parse(&s)
        .unwrap_or_else(|_| gossamer_std::math::big::Uint::from_u64(0))
}

pub(crate) fn builtin_big_int_from_str(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("0").to_string();
    match gossamer_std::math::big::Int::parse(&s) {
        Ok(n) => Ok(ok_variant(Value::String(n.to_string().into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_big_int_from_i64(args: &[Value]) -> RuntimeResult<Value> {
    let n = args.first().and_then(value_to_int).unwrap_or(0);
    let big = gossamer_std::math::big::Int::from_i64(n);
    Ok(Value::String(big.to_string().into()))
}

pub(crate) fn builtin_big_int_to_str(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::String(big_int_from_value(v).to_string().into()))
}

pub(crate) fn builtin_big_int_to_hex(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::String(big_int_from_value(v).to_hex().into()))
}

pub(crate) fn builtin_big_int_to_i64(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    match big_int_from_value(v).to_i64() {
        Some(n) => Ok(some_variant(Value::Int(n))),
        None => Ok(none_variant()),
    }
}

pub(crate) fn builtin_big_int_is_zero(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::Bool(big_int_from_value(v).is_zero()))
}

pub(crate) fn builtin_big_int_is_positive(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::Bool(big_int_from_value(v).is_positive()))
}

pub(crate) fn builtin_big_int_is_negative(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::Bool(big_int_from_value(v).is_negative()))
}

pub(crate) fn builtin_big_int_add(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_int_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_int_from_value(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::String(a.add(&b).to_string().into()))
}

pub(crate) fn builtin_big_int_sub(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_int_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_int_from_value(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::String(a.sub(&b).to_string().into()))
}

pub(crate) fn builtin_big_int_mul(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_int_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_int_from_value(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::String(a.mul(&b).to_string().into()))
}

pub(crate) fn builtin_big_int_div(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_int_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_int_from_value(args.get(1).unwrap_or(&Value::Unit));
    match a.div(&b) {
        Ok(n) => Ok(ok_variant(Value::String(n.to_string().into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_big_int_rem(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_int_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_int_from_value(args.get(1).unwrap_or(&Value::Unit));
    match a.rem(&b) {
        Ok(n) => Ok(ok_variant(Value::String(n.to_string().into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_big_int_pow(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_int_from_value(args.first().unwrap_or(&Value::Unit));
    let exp = args.get(1).and_then(value_to_int).unwrap_or(0).max(0) as u32;
    Ok(Value::String(a.pow(exp).to_string().into()))
}

pub(crate) fn builtin_big_int_abs(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::String(
        big_int_from_value(v).abs().to_string().into(),
    ))
}

pub(crate) fn builtin_big_int_neg(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::String(
        big_int_from_value(v).neg().to_string().into(),
    ))
}

pub(crate) fn builtin_big_int_gcd(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_int_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_int_from_value(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::String(a.gcd(&b).to_string().into()))
}

pub(crate) fn builtin_big_int_lcm(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_int_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_int_from_value(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::String(a.lcm(&b).to_string().into()))
}

pub(crate) fn builtin_big_int_cmp(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_int_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_int_from_value(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::Int(a.compare(&b)))
}

pub(crate) fn builtin_big_uint_from_str(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("0").to_string();
    match gossamer_std::math::big::Uint::parse(&s) {
        Ok(n) => Ok(ok_variant(Value::String(n.to_string().into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_big_uint_from_u64(args: &[Value]) -> RuntimeResult<Value> {
    let n = args.first().and_then(value_to_int).unwrap_or(0).max(0) as u64;
    let big = gossamer_std::math::big::Uint::from_u64(n);
    Ok(Value::String(big.to_string().into()))
}

pub(crate) fn builtin_big_uint_to_str(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::String(big_uint_from_value(v).to_string().into()))
}

pub(crate) fn builtin_big_uint_to_hex(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::String(big_uint_from_value(v).to_hex().into()))
}

pub(crate) fn builtin_big_uint_to_u64(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    match big_uint_from_value(v).to_u64() {
        Some(n) => Ok(some_variant(Value::Int(n as i64))),
        None => Ok(none_variant()),
    }
}

pub(crate) fn builtin_big_uint_is_zero(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::Bool(big_uint_from_value(v).is_zero()))
}

pub(crate) fn builtin_big_uint_add(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_uint_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_uint_from_value(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::String(a.add(&b).to_string().into()))
}

pub(crate) fn builtin_big_uint_mul(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_uint_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_uint_from_value(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::String(a.mul(&b).to_string().into()))
}

pub(crate) fn builtin_big_uint_pow(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_uint_from_value(args.first().unwrap_or(&Value::Unit));
    let exp = args.get(1).and_then(value_to_int).unwrap_or(0).max(0) as u32;
    Ok(Value::String(a.pow(exp).to_string().into()))
}

pub(crate) fn builtin_big_uint_pow_mod(args: &[Value]) -> RuntimeResult<Value> {
    let base = big_uint_from_value(args.first().unwrap_or(&Value::Unit));
    let exp = big_uint_from_value(args.get(1).unwrap_or(&Value::Unit));
    let modulus = big_uint_from_value(args.get(2).unwrap_or(&Value::Unit));
    Ok(Value::String(
        base.pow_mod(&exp, &modulus).to_string().into(),
    ))
}

pub(crate) fn builtin_big_uint_bit_len(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::Int(big_uint_from_value(v).bit_len() as i64))
}

pub(crate) fn builtin_big_factorial(args: &[Value]) -> RuntimeResult<Value> {
    let n = args.first().and_then(value_to_int).unwrap_or(0).max(0) as u64;
    Ok(Value::String(
        gossamer_std::math::big::factorial(n).to_string().into(),
    ))
}

// ---------------------------------------------------------------------------
// 0.4.0 HTTP-module bridges (interp tier).
//
// Free-function-style entry points for the stateless / single-call
// modules. Stateful types that need cross-call state (Router,
// FileServer, WebSocket, Proxy, NativeClient) use a thread-local
// registry keyed by an i64 handle — same shape the existing atomic
// / mutex / scanner bridges use, so the dispatch path is uniform.
// ---------------------------------------------------------------------------
