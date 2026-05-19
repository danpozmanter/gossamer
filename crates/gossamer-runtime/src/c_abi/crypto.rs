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

use std::ffi::CStr;
use std::os::raw::c_char;

/// Returns SHA-256 of the input c-string as lowercase hex.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sha256_hex(input: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let bytes: &[u8] = if input.is_null() {
            &[]
        } else {
            unsafe { CStr::from_ptr(input).to_bytes() }
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
            unsafe { CStr::from_ptr(input).to_bytes() }
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
            unsafe { CStr::from_ptr(input).to_bytes() }
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
            unsafe { CStr::from_ptr(key).to_bytes() }
        };
        let msg_bytes: &[u8] = if message.is_null() {
            &[]
        } else {
            unsafe { CStr::from_ptr(message).to_bytes() }
        };
        let mac = hmac_sha256(key_bytes, msg_bytes);
        super::string::alloc_cstring(hex_encode(&mac).as_bytes())
    })
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
