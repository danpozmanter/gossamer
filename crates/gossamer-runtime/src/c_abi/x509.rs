#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]

//! `std::crypto::x509::parse_pem` leaf intrinsic. Returns a 7-slot
//! `(subject, issuer, serial, not_before_unix, not_after_unix,
//! san_dns, sha256)` tuple via the by-value-aggregate ABI; the
//! injected Gossamer wrapper folds it into a real `CertInfo` struct.
//! Mirrors `gossamer_std::crypto::x509::parse_der` so the compiled
//! tier matches the VM byte-for-byte.

use std::os::raw::c_char;

use sha2::{Digest, Sha256};

use super::string::alloc_cstring;
use super::vec::{GosVec, gos_rt_result_new, gos_rt_vec_push, gos_rt_vec_with_capacity};

unsafe fn cstr<'a>(p: *const c_char) -> &'a str {
    if p.is_null() {
        ""
    } else {
        unsafe { std::ffi::CStr::from_ptr(p) }
            .to_str()
            .unwrap_or("")
    }
}

fn byte_vec(bytes: &[u8]) -> *mut GosVec {
    let v = unsafe { gos_rt_vec_with_capacity(8, bytes.len() as i64) };
    for &b in bytes {
        let pv = i64::from(b);
        unsafe { gos_rt_vec_push(v, std::ptr::addr_of!(pv).cast::<u8>()) };
    }
    v
}

// STRING-typed: the vec owns each element, so `gos_rt_vec_free`
// deep-frees them.
fn str_vec(items: &[String]) -> *mut GosVec {
    let v = unsafe {
        crate::c_abi::vec::gos_rt_vec_with_capacity_typed(
            8,
            items.len() as i64,
            crate::c_abi::vec::vec_elem_kind::STRING,
        )
    };
    for s in items {
        let pv = alloc_cstring(s.as_bytes()) as i64;
        unsafe { gos_rt_vec_push(v, std::ptr::addr_of!(pv).cast::<u8>()) };
    }
    v
}

fn err(msg: &str) -> i128 {
    let cs = std::ffi::CString::new(msg).unwrap_or_default();
    let e = unsafe { super::errors::gos_rt_error_new(cs.as_ptr()) };
    unsafe { gos_rt_result_new(1, e as i64) }
}

/// `crypto::x509::parse_pem(s)` leaf -> Result<(subject, issuer,
/// serial, not_before_unix, not_after_unix, san_dns, sha256), Error>.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_x509_parse_pem_raw(s: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let pem = unsafe { cstr(s) }.as_bytes();
        let der = match x509_parser::pem::parse_x509_pem(pem) {
            Ok((_, p)) => p.contents,
            Err(e) => return err(&format!("x509: pem: {e}")),
        };
        let cert = match x509_parser::parse_x509_certificate(&der) {
            Ok((_, c)) => c,
            Err(e) => return err(&format!("x509: der: {e}")),
        };
        let subject = cert.subject().to_string();
        let issuer = cert.issuer().to_string();
        let serial = cert.serial.to_bytes_be();
        let not_before = cert.validity().not_before.timestamp();
        let not_after = cert.validity().not_after.timestamp();
        let mut san_dns: Vec<String> = Vec::new();
        if let Ok(Some(san)) = cert.subject_alternative_name() {
            for name in &san.value.general_names {
                if let x509_parser::extensions::GeneralName::DNSName(d) = name {
                    san_dns.push((*d).to_string());
                }
            }
        }
        let sha256 = Sha256::digest(&der);

        let blob = crate::c_abi::gos_rt_gc_alloc(56) as *mut i64;
        if blob.is_null() {
            return err("x509: alloc failed");
        }
        unsafe {
            *blob = alloc_cstring(subject.as_bytes()) as i64;
            *blob.add(1) = alloc_cstring(issuer.as_bytes()) as i64;
            *blob.add(2) = byte_vec(&serial) as i64;
            *blob.add(3) = not_before;
            *blob.add(4) = not_after;
            *blob.add(5) = str_vec(&san_dns) as i64;
            *blob.add(6) = byte_vec(&sha256) as i64;
        }
        unsafe { gos_rt_result_new(0, blob as i64) }
    })
}
