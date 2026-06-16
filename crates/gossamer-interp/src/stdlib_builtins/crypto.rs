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

pub(crate) fn install_crypto(globals: &mut Vec<(&'static str, Value)>) {
    // crypto::sha256
    for (short, call) in [
        ("digest", builtin_crypto_sha256_digest as BuiltinFnPub),
        ("hex", builtin_crypto_sha256_hex),
    ] {
        let q: &'static str = Box::leak(format!("crypto::sha256::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
    // crypto::sha512
    {
        let q = "crypto::sha512::hex";
        globals.push((
            q,
            crate::builtins::builtin_pub(q, builtin_crypto_sha512_hex),
        ));
    }
    // crypto::blake3
    {
        let q = "crypto::blake3::hex";
        globals.push((
            q,
            crate::builtins::builtin_pub(q, builtin_crypto_blake3_hex),
        ));
    }
    // crypto::hmac
    {
        let q = "crypto::hmac::sha256_mac";
        globals.push((
            q,
            crate::builtins::builtin_pub(q, builtin_crypto_hmac_sha256_mac),
        ));
    }
    {
        let q = "crypto::hmac::sha256_hex";
        globals.push((
            q,
            crate::builtins::builtin_pub(q, builtin_crypto_hmac_sha256_hex),
        ));
    }
    // crypto::subtle
    {
        let q = "crypto::subtle::constant_time_eq";
        globals.push((
            q,
            crate::builtins::builtin_pub(q, builtin_crypto_subtle_ct_eq),
        ));
    }
    // crypto::rand
    {
        let q = "crypto::rand::bytes";
        globals.push((
            q,
            crate::builtins::builtin_pub(q, builtin_crypto_rand_bytes),
        ));
    }
}

pub(crate) fn bytes_to_value_array(b: &[u8]) -> Value {
    Value::Array(Arc::new(
        b.iter().map(|&x| Value::Int(i64::from(x))).collect(),
    ))
}

pub(crate) fn value_to_bytes(v: &Value) -> Vec<u8> {
    match v {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|x| value_to_int(x).map(|n| n as u8))
            .collect(),
        // An `[u8]` / `[i64]` byte-array literal lowers to the packed
        // `IntArray` representation, not a boxed `Array`; without this
        // arm `value_to_bytes` returned empty and every crypto helper
        // taking `[u8]` (kdf / aead / ed25519) silently hashed nothing.
        Value::IntArray(data) => data.iter().map(|n| *n as u8).collect(),
        Value::String(s) => s.as_str().as_bytes().to_vec(),
        _ => Vec::new(),
    }
}

pub(crate) fn builtin_crypto_sha256_digest(args: &[Value]) -> RuntimeResult<Value> {
    let input = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let digest = gossamer_std::crypto::sha256::digest(&input);
    Ok(bytes_to_value_array(&digest))
}

pub(crate) fn builtin_crypto_sha256_hex(args: &[Value]) -> RuntimeResult<Value> {
    let input = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    Ok(Value::String(
        gossamer_std::crypto::sha256::hex(&input).into(),
    ))
}

pub(crate) fn builtin_crypto_hmac_sha256_mac(args: &[Value]) -> RuntimeResult<Value> {
    let key = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let msg = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let mac = gossamer_std::crypto::hmac::sha256_mac(&key, &msg);
    Ok(bytes_to_value_array(&mac))
}

pub(crate) fn builtin_crypto_hmac_sha256_hex(args: &[Value]) -> RuntimeResult<Value> {
    let key = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let msg = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let mac = gossamer_std::crypto::hmac::sha256_mac(&key, &msg);
    let mut hex = String::with_capacity(mac.len() * 2);
    for b in mac {
        let hi = b >> 4;
        let lo = b & 0x0f;
        for nibble in [hi, lo] {
            let c = match nibble {
                0..=9 => (b'0' + nibble) as char,
                10..=15 => (b'a' + (nibble - 10)) as char,
                _ => '?',
            };
            hex.push(c);
        }
    }
    Ok(Value::String(hex.into()))
}

pub(crate) fn builtin_crypto_sha512_hex(args: &[Value]) -> RuntimeResult<Value> {
    let input = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    Ok(Value::String(
        gossamer_std::crypto::sha512::hex(&input).into(),
    ))
}

pub(crate) fn builtin_crypto_blake3_hex(args: &[Value]) -> RuntimeResult<Value> {
    let input = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    Ok(Value::String(
        gossamer_std::crypto::blake3::hex(&input).into(),
    ))
}

pub(crate) fn builtin_crypto_subtle_ct_eq(args: &[Value]) -> RuntimeResult<Value> {
    let a = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let b = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::Bool(gossamer_std::crypto::subtle::constant_time_eq(
        &a, &b,
    )))
}

pub(crate) fn builtin_crypto_rand_bytes(args: &[Value]) -> RuntimeResult<Value> {
    let n = args.first().and_then(value_to_int).unwrap_or(0);
    let n = usize::try_from(n.max(0)).unwrap_or(0);
    match gossamer_std::crypto::rand::bytes(n) {
        Ok(b) => Ok(ok_variant(bytes_to_value_array(&b))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// encoding::yaml (always enabled in this crate)
