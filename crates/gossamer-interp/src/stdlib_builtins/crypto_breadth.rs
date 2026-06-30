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

pub(crate) fn install_crypto_breadth(globals: &mut Vec<(&'static str, Value)>) {
    // sha512
    for (short, call) in [
        ("digest", builtin_crypto_sha512_digest as BuiltinFnPub),
        ("hex", builtin_crypto_sha512_hex),
    ] {
        let q: &'static str = Box::leak(format!("crypto::sha512::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
    // blake3
    for (short, call) in [
        ("digest", builtin_crypto_blake3_digest as BuiltinFnPub),
        ("hex", builtin_crypto_blake3_hex),
    ] {
        let q: &'static str = Box::leak(format!("crypto::blake3::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
    // aead
    for (short, call) in [
        ("aes_256_gcm_seal", builtin_crypto_aes_seal as BuiltinFnPub),
        ("aes_256_gcm_open", builtin_crypto_aes_open),
        ("chacha20_poly1305_seal", builtin_crypto_chacha_seal),
        ("chacha20_poly1305_open", builtin_crypto_chacha_open),
    ] {
        let q: &'static str = Box::leak(format!("crypto::aead::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
    // ed25519
    for (short, call) in [
        ("keypair", builtin_crypto_ed25519_keypair as BuiltinFnPub),
        ("sign", builtin_crypto_ed25519_sign),
        ("verify", builtin_crypto_ed25519_verify),
    ] {
        let q: &'static str = Box::leak(format!("crypto::ed25519::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
    // ecdsa
    for (short, call) in [
        (
            "keypair_pem",
            builtin_crypto_ecdsa_keypair_pem as BuiltinFnPub,
        ),
        ("sign_pem", builtin_crypto_ecdsa_sign_pem),
        ("verify_pem", builtin_crypto_ecdsa_verify_pem),
    ] {
        let q: &'static str = Box::leak(format!("crypto::ecdsa::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
    // kdf
    for (short, call) in [
        ("pbkdf2_sha256", builtin_crypto_kdf_pbkdf2 as BuiltinFnPub),
        ("argon2id_hash", builtin_crypto_kdf_argon2id_hash),
        ("argon2id_verify", builtin_crypto_kdf_argon2id_verify),
        ("scrypt_interactive", builtin_crypto_kdf_scrypt),
    ] {
        let q: &'static str = Box::leak(format!("crypto::kdf::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
    // password (Argon2id PHC strings; delegates to kdf::argon2id)
    for (short, call) in [
        ("hash", builtin_crypto_password_hash as BuiltinFnPub),
        ("verify", builtin_crypto_password_verify),
        ("needs_rehash", builtin_crypto_password_needs_rehash),
    ] {
        let q: &'static str = Box::leak(format!("crypto::password::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
    // x509
    {
        let q = "crypto::x509::parse_pem";
        globals.push((
            q,
            crate::builtins::builtin_pub(q, builtin_crypto_x509_parse_pem),
        ));
    }
    // Leaf intrinsic for the injected real-struct `CertInfo` wrapper:
    // returns the fields as a 7-tuple the wrapper folds into a struct.
    {
        let q = "__gos_x509_parse_pem_raw";
        globals.push((
            q,
            crate::builtins::builtin_pub(q, builtin_x509_parse_pem_raw),
        ));
    }
}

pub(crate) fn builtin_x509_parse_pem_raw(args: &[Value]) -> RuntimeResult<Value> {
    let pem = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::crypto::x509::parse_pem(&pem) {
        Ok(info) => {
            let serial = Value::Array(Arc::new(
                info.serial
                    .into_iter()
                    .map(|b| Value::Int(i64::from(b)))
                    .collect(),
            ));
            let san = Value::Array(Arc::new(
                info.san_dns
                    .into_iter()
                    .map(|s| Value::String(s.into()))
                    .collect(),
            ));
            let sha = Value::Array(Arc::new(
                info.sha256
                    .iter()
                    .map(|b| Value::Int(i64::from(*b)))
                    .collect(),
            ));
            Ok(ok_variant(Value::Tuple(Arc::from(vec![
                Value::String(info.subject.into()),
                Value::String(info.issuer.into()),
                serial,
                Value::Int(info.not_before_unix),
                Value::Int(info.not_after_unix),
                san,
                sha,
            ]))))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_crypto_sha512_digest(args: &[Value]) -> RuntimeResult<Value> {
    let input = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let digest = gossamer_std::crypto::sha512::digest(&input);
    Ok(bytes_to_value_array(&digest))
}

pub(crate) fn builtin_crypto_sha512_hex(args: &[Value]) -> RuntimeResult<Value> {
    let input = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    Ok(Value::String(
        gossamer_std::crypto::sha512::hex(&input).into(),
    ))
}

pub(crate) fn builtin_crypto_blake3_digest(args: &[Value]) -> RuntimeResult<Value> {
    let input = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let digest = gossamer_std::crypto::blake3::digest(&input);
    Ok(bytes_to_value_array(&digest))
}

pub(crate) fn builtin_crypto_blake3_hex(args: &[Value]) -> RuntimeResult<Value> {
    let input = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    Ok(Value::String(
        gossamer_std::crypto::blake3::hex(&input).into(),
    ))
}

pub(crate) fn builtin_crypto_aes_seal(args: &[Value]) -> RuntimeResult<Value> {
    let key = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let nonce = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let pt = value_to_bytes(args.get(2).unwrap_or(&Value::Unit));
    let aad = value_to_bytes(args.get(3).unwrap_or(&Value::Unit));
    match gossamer_std::crypto::aead::aes_256_gcm_seal(&key, &nonce, &pt, &aad) {
        Ok(ct) => Ok(ok_variant(bytes_to_value_array(&ct))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_crypto_aes_open(args: &[Value]) -> RuntimeResult<Value> {
    let key = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let nonce = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let ct = value_to_bytes(args.get(2).unwrap_or(&Value::Unit));
    let aad = value_to_bytes(args.get(3).unwrap_or(&Value::Unit));
    match gossamer_std::crypto::aead::aes_256_gcm_open(&key, &nonce, &ct, &aad) {
        Ok(pt) => Ok(ok_variant(bytes_to_value_array(&pt))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_crypto_chacha_seal(args: &[Value]) -> RuntimeResult<Value> {
    let key = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let nonce = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let pt = value_to_bytes(args.get(2).unwrap_or(&Value::Unit));
    let aad = value_to_bytes(args.get(3).unwrap_or(&Value::Unit));
    match gossamer_std::crypto::aead::chacha20_poly1305_seal(&key, &nonce, &pt, &aad) {
        Ok(ct) => Ok(ok_variant(bytes_to_value_array(&ct))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_crypto_chacha_open(args: &[Value]) -> RuntimeResult<Value> {
    let key = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let nonce = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let ct = value_to_bytes(args.get(2).unwrap_or(&Value::Unit));
    let aad = value_to_bytes(args.get(3).unwrap_or(&Value::Unit));
    match gossamer_std::crypto::aead::chacha20_poly1305_open(&key, &nonce, &ct, &aad) {
        Ok(pt) => Ok(ok_variant(bytes_to_value_array(&pt))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_crypto_ed25519_keypair(_args: &[Value]) -> RuntimeResult<Value> {
    match gossamer_std::crypto::ed25519::keypair() {
        Ok((secret, public)) => Ok(ok_variant(Value::Tuple(Arc::from(vec![
            bytes_to_value_array(&secret),
            bytes_to_value_array(&public),
        ])))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_crypto_ed25519_sign(args: &[Value]) -> RuntimeResult<Value> {
    let secret = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let msg = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    match gossamer_std::crypto::ed25519::sign(&secret, &msg) {
        Ok(sig) => Ok(ok_variant(bytes_to_value_array(&sig))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_crypto_ed25519_verify(args: &[Value]) -> RuntimeResult<Value> {
    let public = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let msg = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let sig = value_to_bytes(args.get(2).unwrap_or(&Value::Unit));
    match gossamer_std::crypto::ed25519::verify(&public, &msg, &sig) {
        Ok(()) => Ok(ok_variant(Value::Unit)),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_crypto_ecdsa_keypair_pem(_args: &[Value]) -> RuntimeResult<Value> {
    match gossamer_std::crypto::ecdsa::keypair_pem() {
        Ok((secret, public)) => Ok(ok_variant(Value::Tuple(Arc::from(vec![
            Value::String(secret.into()),
            Value::String(public.into()),
        ])))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_crypto_ecdsa_sign_pem(args: &[Value]) -> RuntimeResult<Value> {
    let secret = args.first().and_then(as_str).unwrap_or("").to_string();
    let msg = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    match gossamer_std::crypto::ecdsa::sign_pem(&secret, &msg) {
        Ok(sig) => Ok(ok_variant(bytes_to_value_array(&sig))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_crypto_ecdsa_verify_pem(args: &[Value]) -> RuntimeResult<Value> {
    let public = args.first().and_then(as_str).unwrap_or("").to_string();
    let msg = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let sig = value_to_bytes(args.get(2).unwrap_or(&Value::Unit));
    match gossamer_std::crypto::ecdsa::verify_pem(&public, &msg, &sig) {
        Ok(()) => Ok(ok_variant(Value::Unit)),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_crypto_kdf_pbkdf2(args: &[Value]) -> RuntimeResult<Value> {
    let password = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let salt = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let iterations = args.get(2).and_then(value_to_int).unwrap_or(100_000) as u32;
    let output = args.get(3).and_then(value_to_int).unwrap_or(32) as usize;
    let key = gossamer_std::crypto::kdf::pbkdf2_sha256(&password, &salt, iterations, output);
    Ok(bytes_to_value_array(&key))
}

pub(crate) fn builtin_crypto_kdf_argon2id_hash(args: &[Value]) -> RuntimeResult<Value> {
    let password = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::crypto::kdf::argon2id_hash(&password) {
        Ok(phc) => Ok(ok_variant(Value::String(phc.into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_crypto_kdf_argon2id_verify(args: &[Value]) -> RuntimeResult<Value> {
    let password = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let phc = args.get(1).and_then(as_str).unwrap_or("").to_string();
    match gossamer_std::crypto::kdf::argon2id_verify(&password, &phc) {
        Ok(ok) => Ok(ok_variant(Value::Bool(ok))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_crypto_password_hash(args: &[Value]) -> RuntimeResult<Value> {
    let password = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::crypto::password::hash(&password) {
        Ok(phc) => Ok(ok_variant(Value::String(phc.into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_crypto_password_verify(args: &[Value]) -> RuntimeResult<Value> {
    let password = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let phc = args.get(1).and_then(as_str).unwrap_or("").to_string();
    match gossamer_std::crypto::password::verify(&password, &phc) {
        Ok(ok) => Ok(ok_variant(Value::Bool(ok))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_crypto_password_needs_rehash(args: &[Value]) -> RuntimeResult<Value> {
    let phc = args.first().and_then(as_str).unwrap_or("").to_string();
    Ok(Value::Bool(gossamer_std::crypto::password::needs_rehash(
        &phc,
    )))
}

pub(crate) fn builtin_crypto_kdf_scrypt(args: &[Value]) -> RuntimeResult<Value> {
    let password = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let salt = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let output = args.get(2).and_then(value_to_int).unwrap_or(32) as usize;
    match gossamer_std::crypto::kdf::scrypt_interactive(&password, &salt, output) {
        Ok(key) => Ok(ok_variant(bytes_to_value_array(&key))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_crypto_x509_parse_pem(args: &[Value]) -> RuntimeResult<Value> {
    let pem = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::crypto::x509::parse_pem(&pem) {
        Ok(info) => {
            let san_v = Value::Array(Arc::new(
                info.san_dns
                    .into_iter()
                    .map(|s| Value::String(s.into()))
                    .collect(),
            ));
            let struct_v = Value::struct_(
                "crypto::x509::CertInfo",
                Arc::unwrap_or_clone(Arc::new(vec![
                    ("subject", Value::String(info.subject.into())),
                    ("issuer", Value::String(info.issuer.into())),
                    ("serial", bytes_to_value_array(&info.serial)),
                    ("not_before_unix", Value::Int(info.not_before_unix)),
                    ("not_after_unix", Value::Int(info.not_after_unix)),
                    ("san_dns", san_v),
                    ("sha256", bytes_to_value_array(&info.sha256)),
                ])),
            );
            Ok(ok_variant(struct_v))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// json builtins (parse / encode / encode_pretty / valid)
