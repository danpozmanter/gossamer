//! C-ABI dispatch shims for `std::crypto::*`. Each helper takes a
//! NUL-terminated c-string input and returns a freshly-allocated
//! NUL-terminated hex c-string (the canonical wire shape for
//! crypto digests through the Gossamer String surface). Returning
//! hex rather than raw bytes lets compiled programs route through
//! `gos_rt_str_*` without needing a separate `Bytes` shape.

#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::cast_possible_truncation)]

use std::os::raw::c_char;

use super::vec::GosVec;

/// Returns SHA-256 of the input c-string as lowercase hex.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sha256_hex(input: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let bytes: &[u8] = if input.is_null() {
            &[]
        } else {
            unsafe { crate::c_abi::gos_str_arg_bytes(input) }
        };
        let hex = gossamer_pkg::sha256::hex(bytes);
        super::string::alloc_cstring(hex.as_bytes())
    })
}

/// Returns SHA-512 of the input c-string as lowercase hex.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sha512_hex(input: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        use sha2::Digest;
        let bytes: &[u8] = if input.is_null() {
            &[]
        } else {
            unsafe { crate::c_abi::gos_str_arg_bytes(input) }
        };
        let mut h = sha2::Sha512::new();
        h.update(bytes);
        let digest: [u8; 64] = h.finalize().into();
        super::string::alloc_cstring(hex_encode(&digest).as_bytes())
    })
}

/// Returns BLAKE3 of the input c-string as lowercase hex.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_blake3_hex(input: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let bytes: &[u8] = if input.is_null() {
            &[]
        } else {
            unsafe { crate::c_abi::gos_str_arg_bytes(input) }
        };
        let mut hasher = ::blake3::Hasher::new();
        hasher.update(bytes);
        let digest: [u8; 32] = *hasher.finalize().as_bytes();
        super::string::alloc_cstring(hex_encode(&digest).as_bytes())
    })
}

/// Returns HMAC-SHA256 of `message` keyed by `key`, hex-encoded.
/// Reference: RFC 2104. Mirrors `std::crypto::hmac::sha256_mac`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_hmac_sha256_hex(
    key: *const c_char,
    message: *const c_char,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let key_bytes: &[u8] = if key.is_null() {
            &[]
        } else {
            unsafe { crate::c_abi::gos_str_arg_bytes(key) }
        };
        let msg_bytes: &[u8] = if message.is_null() {
            &[]
        } else {
            unsafe { crate::c_abi::gos_str_arg_bytes(message) }
        };
        let mac = hmac_sha256(key_bytes, msg_bytes);
        super::string::alloc_cstring(hex_encode(&mac).as_bytes())
    })
}

/// `crypto::hmac::sha256_mac(key, message) -> [u8]` - the raw 32-byte
/// MAC as a byte vector (vs the hex string from `sha256_hex`). Both
/// arguments are Gossamer `[u8]` values (`GosVec`), not c-strings:
/// the MAC must cover arbitrary binary key / message bytes, including
/// embedded NUL, exactly as the interp builtin's `value_to_bytes`
/// extraction does.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_crypto_hmac_sha256_mac(
    key: *const GosVec,
    message: *const GosVec,
) -> *mut GosVec {
    let key_bytes = unsafe { super::encoding::gosvec_u8(key) };
    let msg_bytes = unsafe { super::encoding::gosvec_u8(message) };
    let mac = hmac_sha256(&key_bytes, &msg_bytes);
    super::encoding::bytes_to_gosvec(&mac)
}

/// `crypto::sha256::digest(data: &[u8]) -> [u8; 32]` - the raw 32-byte
/// digest (not the hex string). The argument is a Gossamer `[u8]`
/// (`GosVec`), so the digest covers arbitrary binary input including
/// embedded NUL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_crypto_sha256_digest(input: *const GosVec) -> *mut GosVec {
    let bytes = unsafe { super::encoding::gosvec_u8(input) };
    super::encoding::bytes_to_gosvec(&gossamer_pkg::sha256::digest(&bytes))
}

/// `crypto::sha512::digest(data: &[u8]) -> [u8; 64]` - the raw 64-byte
/// digest (not the hex string), mirroring `gos_rt_crypto_sha256_digest`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_crypto_sha512_digest(input: *const GosVec) -> *mut GosVec {
    use sha2::Digest;
    let bytes = unsafe { super::encoding::gosvec_u8(input) };
    let mut h = sha2::Sha512::new();
    h.update(&bytes);
    let digest: [u8; 64] = h.finalize().into();
    super::encoding::bytes_to_gosvec(&digest)
}

/// `crypto::blake3::digest(data: &[u8]) -> [u8; 32]` - the raw 32-byte
/// digest (not the hex string), mirroring `gos_rt_crypto_sha256_digest`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_crypto_blake3_digest(input: *const GosVec) -> *mut GosVec {
    let bytes = unsafe { super::encoding::gosvec_u8(input) };
    let mut hasher = ::blake3::Hasher::new();
    hasher.update(&bytes);
    let digest: [u8; 32] = *hasher.finalize().as_bytes();
    super::encoding::bytes_to_gosvec(&digest)
}

/// Reference HMAC-SHA256 over arbitrary `key` / `message`. Kept
/// in the runtime so the compiled tier doesn't need to round-
/// trip through `gossamer-std`. RFC 2104 block size for SHA-256
/// is 64 bytes; longer keys are pre-hashed.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block: [u8; BLOCK] = [0; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&gossamer_pkg::sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad: [u8; BLOCK] = [0x36; BLOCK];
    let mut outer_pad: [u8; BLOCK] = [0x5c; BLOCK];
    for i in 0..BLOCK {
        inner_pad[i] ^= block[i];
        outer_pad[i] ^= block[i];
    }
    let mut inner_input = Vec::with_capacity(BLOCK + message.len());
    inner_input.extend_from_slice(&inner_pad);
    inner_input.extend_from_slice(message);
    let inner_hash = gossamer_pkg::sha256::digest(&inner_input);
    let mut outer_input = Vec::with_capacity(BLOCK + inner_hash.len());
    outer_input.extend_from_slice(&outer_pad);
    outer_input.extend_from_slice(&inner_hash);
    gossamer_pkg::sha256::digest(&outer_input)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(nibble_char(b >> 4));
        out.push(nibble_char(b & 0x0f));
    }
    out
}

fn nibble_char(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + (n - 10)) as char,
        _ => '?',
    }
}

fn crypto_err(msg: &str, fallback: &str) -> i128 {
    let cs = std::ffi::CString::new(msg)
        .unwrap_or_else(|_| std::ffi::CString::new(fallback).expect("static"));
    let err = unsafe { super::errors::gos_rt_error_new(cs.as_ptr()) };
    super::vec::gos_rt_result_new(1, err as i64)
}

/// `crypto::rand::bytes(n) -> Result<Vec<u8>, errors::Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_crypto_rand_bytes(n: i64) -> i128 {
    if n < 0 {
        return crypto_err(
            "crypto::rand::bytes: count must be non-negative",
            "crypto::rand error",
        );
    }
    let len = n;
    let v = unsafe { super::vec::gos_rt_vec_with_capacity(1, len) };
    if len == 0 {
        return super::vec::gos_rt_result_new(0, v as i64);
    }
    // Fill the freshly-allocated buffer with OS randomness and pin
    // `len` so `b.len()` reads `n`. Unsafe is justified because GosVec
    // exposes its backing buffer as a raw pointer at the C ABI
    // boundary, and there is no safe Rust way to fill it.
    let vref = unsafe { &mut *v };
    if !vref.ptr.is_null() {
        let slice = unsafe { std::slice::from_raw_parts_mut(vref.ptr.as_ptr(), len as usize) };
        if getrandom::fill(slice).is_err() {
            return crypto_err("crypto::rand: rng failure", "crypto::rand error");
        }
    }
    vref.len = len;
    super::vec::gos_rt_result_new(0, v as i64)
}

/// Packs an `Err(errors::Error)` for the `crypto::password` Result-
/// returning shims.
fn password_err(msg: &str) -> i128 {
    crypto_err(msg, "crypto::password error")
}

/// `crypto::password::hash(plaintext) -> Result<String, errors::Error>` -
/// Argon2id PHC hash. Same defaults as `kdf::argon2id_hash`
/// (Argon2id, V0x13, `Params::default()`), so a hash minted on one
/// tier verifies on another.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_crypto_password_hash(plaintext: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        use argon2::password_hash::{PasswordHasher, SaltString};
        use argon2::{Algorithm, Argon2, Params, Version};
        let pw = if plaintext.is_null() {
            Vec::new()
        } else {
            unsafe { crate::c_abi::gos_str_arg_bytes(plaintext) }.to_vec()
        };
        let mut salt_bytes = [0u8; 16];
        if getrandom::fill(&mut salt_bytes).is_err() {
            return password_err("crypto::password: rng failure");
        }
        let salt = match SaltString::encode_b64(&salt_bytes) {
            Ok(s) => s,
            Err(e) => return password_err(&format!("crypto::password: {e}")),
        };
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::default());
        match argon.hash_password(&pw, &salt) {
            Ok(h) => super::vec::gos_rt_result_new(
                0,
                super::string::alloc_cstring(h.to_string().as_bytes()) as i64,
            ),
            Err(e) => password_err(&format!("crypto::password: {e}")),
        }
    })
}

/// `crypto::password::verify(plaintext, phc) -> Result<bool, errors::Error>` -
/// constant-time-ish verify using the parameters embedded in the
/// PHC string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_crypto_password_verify(
    plaintext: *const c_char,
    phc: *const c_char,
) -> i128 {
    ffi_entry!(0i128, {
        use argon2::Argon2;
        use argon2::password_hash::{PasswordHash, PasswordVerifier};
        let pw = if plaintext.is_null() {
            Vec::new()
        } else {
            unsafe { crate::c_abi::gos_str_arg_bytes(plaintext) }.to_vec()
        };
        let phc_s = if phc.is_null() {
            String::new()
        } else {
            unsafe { crate::c_abi::gos_str_arg_string(phc) }
        };
        let parsed = match PasswordHash::new(&phc_s) {
            Ok(p) => p,
            Err(e) => return password_err(&format!("crypto::password: {e}")),
        };
        let ok = Argon2::default().verify_password(&pw, &parsed).is_ok();
        super::vec::gos_rt_result_new(0, i64::from(ok))
    })
}

/// `crypto::password::needs_rehash(phc) -> bool` - replicates
/// `kdf::needs_rehash` exactly (non-argon2id or weaker-than-default
/// params → true).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_crypto_password_needs_rehash(phc: *const c_char) -> i8 {
    ffi_entry!(0, {
        use argon2::Params;
        use argon2::password_hash::PasswordHash;
        let phc_s = if phc.is_null() {
            String::new()
        } else {
            unsafe { crate::c_abi::gos_str_arg_string(phc) }
        };
        let Ok(parsed) = PasswordHash::new(&phc_s) else {
            return 0;
        };
        if !matches!(parsed.algorithm.as_str(), "argon2id") {
            return 1;
        }
        let target = Params::default();
        let Ok(current) = Params::try_from(&parsed) else {
            return 1;
        };
        i8::from(
            current.m_cost() < target.m_cost()
                || current.t_cost() < target.t_cost()
                || current.p_cost() < target.p_cost(),
        )
    })
}
