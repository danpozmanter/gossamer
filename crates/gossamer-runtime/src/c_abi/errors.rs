#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::same_length_and_capacity)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::cast_ptr_alignment)]
#![allow(clippy::ptr_as_ptr)]
#![allow(static_mut_refs)]
#![allow(unused_unsafe)]
#![allow(clippy::wildcard_imports)]

use std::ffi::CStr;
use std::os::raw::c_char;

use super::*;

// ---------------------------------------------------------------
// errors module — Gossamer's `Result<T, errors::Error>` plumbing.
// `Error` is an opaque heap struct: a leaked message string plus
// an optional cause pointer. The compiled tier represents an
// `errors::Error` value as `*mut GosError`; `Option<&Error>`
// (`e.cause()` return) is the same pointer with `null` for
// `None`.
// ---------------------------------------------------------------

#[repr(C)]
pub struct GosError {
    /// Heap-leaked, nul-terminated UTF-8 message.
    pub message: SyncRawPtr<c_char>,
    /// Cause pointer. NULL when the error has no cause.
    pub cause: SyncRawPtr<GosError>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_error_new(msg: *const c_char) -> *mut GosError {
    ffi_entry!(std::ptr::null_mut(), {
        let text = if msg.is_null() {
            Vec::new()
        } else {
            unsafe { CStr::from_ptr(msg).to_bytes().to_vec() }
        };
        let leaked = alloc_cstring(&text);
        Box::into_raw(Box::new(GosError {
            message: SyncRawPtr::new(leaked),
            cause: SyncRawPtr::NULL,
        }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_error_wrap(
    cause: *mut GosError,
    msg: *const c_char,
) -> *mut GosError {
    ffi_entry!(std::ptr::null_mut(), {
        let text = if msg.is_null() {
            Vec::new()
        } else {
            unsafe { CStr::from_ptr(msg).to_bytes().to_vec() }
        };
        let leaked = alloc_cstring(&text);
        Box::into_raw(Box::new(GosError {
            message: SyncRawPtr::new(leaked),
            cause: SyncRawPtr::new(cause),
        }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_error_message(err: *const GosError) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if err.is_null() {
            return alloc_cstring(b"");
        }
        let m = unsafe { (*err).message };
        if m.is_null() {
            return alloc_cstring(b"");
        }
        // Re-leak a copy so the caller can hold the string past the
        // GosError's lifetime if it ever gets reclaimed.
        let bytes = unsafe { CStr::from_ptr(m.as_ptr()).to_bytes().to_vec() };
        alloc_cstring(&bytes)
    })
}
