//! Bytecode-VM builtins for `std::jwt`, exposing the
//! struct-marshalling-free JSON-string entry API. Claims travel as a
//! JSON object string and `verify_*` returns the canonical claims as a
//! JSON object string, so no `Claims` struct crosses a call boundary.
//!
//! Each builtin delegates to a thin `gossamer_std::jwt::*_json` wrapper
//! over the typed `Claims` API; the compiled tier mirrors the same
//! algorithm in `gossamer-runtime/src/c_abi/crypto_jwt.rs`, producing
//! byte-identical tokens across `gos run` / Cranelift / LLVM.

#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::unnecessary_wraps)]

use crate::builtins::{BuiltinFnPub, as_str, err_variant, ok_variant, value_to_int};
use crate::value::{RuntimeResult, Value};

use super::crypto::value_to_bytes;

/// Entry point invoked from `builtins::install`.
pub(crate) fn install_jwt(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        ("sign_hs", builtin_jwt_sign_hs as BuiltinFnPub),
        ("verify_hs", builtin_jwt_verify_hs),
        ("sign_es256", builtin_jwt_sign_es256),
        ("verify_es256", builtin_jwt_verify_es256),
        ("sign_eddsa", builtin_jwt_sign_eddsa),
        ("verify_eddsa", builtin_jwt_verify_eddsa),
    ] {
        let q: &'static str = Box::leak(format!("jwt::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

fn arg_str(args: &[Value], i: usize) -> String {
    as_str(args.get(i).unwrap_or(&Value::Unit))
        .unwrap_or("")
        .to_string()
}

fn arg_leeway(args: &[Value], i: usize) -> i64 {
    value_to_int(args.get(i).unwrap_or(&Value::Unit)).unwrap_or(0)
}

/// `jwt::sign_hs(alg, claims_json, key) -> Result<String, errors::Error>`.
pub(crate) fn builtin_jwt_sign_hs(args: &[Value]) -> RuntimeResult<Value> {
    let alg = arg_str(args, 0);
    let claims = arg_str(args, 1);
    let key = value_to_bytes(args.get(2).unwrap_or(&Value::Unit));
    match gossamer_std::jwt::sign_hs_json(&alg, &claims, &key) {
        Ok(token) => Ok(ok_variant(Value::String(token.into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

/// `jwt::verify_hs(token, alg, key, leeway_secs)
/// -> Result<String, errors::Error>`.
pub(crate) fn builtin_jwt_verify_hs(args: &[Value]) -> RuntimeResult<Value> {
    let token = arg_str(args, 0);
    let alg = arg_str(args, 1);
    let key = value_to_bytes(args.get(2).unwrap_or(&Value::Unit));
    let leeway = arg_leeway(args, 3);
    match gossamer_std::jwt::verify_hs_json(&token, &alg, &key, leeway) {
        Ok(claims) => Ok(ok_variant(Value::String(claims.into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

/// `jwt::sign_es256(claims_json, signing_key_pem)
/// -> Result<String, errors::Error>`.
pub(crate) fn builtin_jwt_sign_es256(args: &[Value]) -> RuntimeResult<Value> {
    let claims = arg_str(args, 0);
    let pem = arg_str(args, 1);
    match gossamer_std::jwt::sign_es256_json(&claims, &pem) {
        Ok(token) => Ok(ok_variant(Value::String(token.into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

/// `jwt::verify_es256(token, verifying_key_pem, leeway_secs)
/// -> Result<String, errors::Error>`.
pub(crate) fn builtin_jwt_verify_es256(args: &[Value]) -> RuntimeResult<Value> {
    let token = arg_str(args, 0);
    let pem = arg_str(args, 1);
    let leeway = arg_leeway(args, 2);
    match gossamer_std::jwt::verify_es256_json(&token, &pem, leeway) {
        Ok(claims) => Ok(ok_variant(Value::String(claims.into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

/// `jwt::sign_eddsa(claims_json, signing_key_pem)
/// -> Result<String, errors::Error>`.
pub(crate) fn builtin_jwt_sign_eddsa(args: &[Value]) -> RuntimeResult<Value> {
    let claims = arg_str(args, 0);
    let pem = arg_str(args, 1);
    match gossamer_std::jwt::sign_eddsa_json(&claims, &pem) {
        Ok(token) => Ok(ok_variant(Value::String(token.into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

/// `jwt::verify_eddsa(token, verifying_key_pem, leeway_secs)
/// -> Result<String, errors::Error>`.
pub(crate) fn builtin_jwt_verify_eddsa(args: &[Value]) -> RuntimeResult<Value> {
    let token = arg_str(args, 0);
    let pem = arg_str(args, 1);
    let leeway = arg_leeway(args, 2);
    match gossamer_std::jwt::verify_eddsa_json(&token, &pem, leeway) {
        Ok(claims) => Ok(ok_variant(Value::String(claims.into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}
