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

/// Canonical `Into<errors::Error>` conversion shim emitted by
/// the HIR `?`-propagation desugar when the inner expression's
/// `Err` type differs from the enclosing function's. Accepts a
/// caller-provided c-string: when the value the caller wants to
/// propagate is already an `errors::Error` the codegen routes
/// the underlying message pointer here; non-Error payloads
/// (most commonly `String`) become a fresh error whose message
/// is the c-string content. Returns a fresh boxed `GosError`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_error_from(value: *const c_char) -> *mut GosError {
    ffi_entry!(std::ptr::null_mut(), {
        let text = if value.is_null() {
            Vec::new()
        } else {
            unsafe { CStr::from_ptr(value).to_bytes().to_vec() }
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

/// Display (`{}`) rendering of an `errors::Error`: the Go-style
/// colon-joined cause chain (`"outer: mid: root"`). `.message()`
/// stays top-level-only via [`gos_rt_error_message`] — this entry
/// is for the format-macro lowering of `{}` on an error value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_error_display(err: *const GosError) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let mut text: Vec<u8> = Vec::new();
        let mut cur = err;
        let mut first = true;
        while !cur.is_null() {
            if !first {
                text.extend_from_slice(b": ");
            }
            first = false;
            let m = unsafe { (*cur).message };
            if !m.is_null() {
                text.extend_from_slice(unsafe { CStr::from_ptr(m.as_ptr()).to_bytes() });
            }
            cur = unsafe { (*cur).cause.as_ptr() };
        }
        alloc_cstring(&text)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(f: unsafe extern "C" fn(*const GosError) -> *mut c_char, e: *mut GosError) -> String {
        let p = unsafe { f(e) };
        let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        unsafe { crate::c_abi::string::gos_rt_str_free(p) };
        s
    }

    #[test]
    fn error_new_displays_message_only() {
        let e = unsafe { gos_rt_error_new(c"boom".as_ptr()) };
        assert_eq!(render(gos_rt_error_display, e), "boom");
        assert_eq!(render(gos_rt_error_message, e), "boom");
    }

    #[test]
    fn wrap_two_deep_displays_colon_joined_chain() {
        let root = unsafe { gos_rt_error_new(c"root".as_ptr()) };
        let mid = unsafe { gos_rt_error_wrap(root, c"mid".as_ptr()) };
        let outer = unsafe { gos_rt_error_wrap(mid, c"outer".as_ptr()) };
        assert_eq!(render(gos_rt_error_display, outer), "outer: mid: root");
    }

    #[test]
    fn wrap_keeps_message_top_level_only() {
        let root = unsafe { gos_rt_error_new(c"root".as_ptr()) };
        let outer = unsafe { gos_rt_error_wrap(root, c"outer".as_ptr()) };
        assert_eq!(render(gos_rt_error_message, outer), "outer");
    }
}
