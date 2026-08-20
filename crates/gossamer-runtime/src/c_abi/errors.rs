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

use std::os::raw::c_char;

use super::*;

// ---------------------------------------------------------------
// errors module - Gossamer's `Result<T, errors::Error>` plumbing.
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
    /// Structured diagnostic fields in insertion order. Only Rust reads
    /// this tail; the compiled tiers carry the whole error as a pointer.
    pub fields: Vec<(String, String)>,
}

/// Builds a causeless error carrying `text` as its message.
///
/// Runtime callers that already hold the message as Rust bytes reach the
/// error this way rather than through a host C string, which the string ABI
/// would have to measure with `strlen`.
pub(crate) fn error_new_from_bytes(text: &[u8]) -> *mut GosError {
    let leaked = alloc_cstring(text);
    Box::into_raw(Box::new(GosError {
        message: SyncRawPtr::new(leaked),
        cause: SyncRawPtr::NULL,
        fields: Vec::new(),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_error_new(msg: *const c_char) -> *mut GosError {
    ffi_entry!(std::ptr::null_mut(), {
        let text = if msg.is_null() {
            Vec::new()
        } else {
            unsafe { crate::c_abi::gos_str_arg_bytes(msg) }.to_vec()
        };
        error_new_from_bytes(&text)
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
            unsafe { crate::c_abi::gos_str_arg_bytes(value) }.to_vec()
        };
        let leaked = alloc_cstring(&text);
        Box::into_raw(Box::new(GosError {
            message: SyncRawPtr::new(leaked),
            cause: SyncRawPtr::NULL,
            fields: Vec::new(),
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
            unsafe { crate::c_abi::gos_str_arg_bytes(msg) }.to_vec()
        };
        let leaked = alloc_cstring(&text);
        Box::into_raw(Box::new(GosError {
            message: SyncRawPtr::new(leaked),
            cause: SyncRawPtr::new(cause),
            fields: Vec::new(),
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
        alloc_cstring(unsafe { crate::c_abi::gos_str_arg_bytes(m.as_ptr()) })
    })
}

/// Display (`{}`) rendering of an `errors::Error`: the Go-style
/// colon-joined cause chain (`"outer: mid: root"`). `.message()`
/// stays top-level-only via [`gos_rt_error_message`] - this entry
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
                text.extend_from_slice(unsafe { crate::c_abi::gos_str_arg_bytes(m.as_ptr()) });
            }
            cur = unsafe { (*cur).cause.as_ptr() };
        }
        alloc_cstring(&text)
    })
}

/// Slot layout of the `errors::Error::fields()` vec: both words are
/// vec-owned strings.
static ERROR_FIELDS_SLOT_CHILDREN: [crate::c_abi::vec::VecSlotChild; 2] = [
    crate::c_abi::vec::VecSlotChild {
        gate: -1,
        disc_word: 0,
        word: 0,
        kind: crate::c_abi::vec::vec_elem_kind::STRING,
    },
    crate::c_abi::vec::VecSlotChild {
        gate: -1,
        disc_word: 0,
        word: 1,
        kind: crate::c_abi::vec::vec_elem_kind::STRING,
    },
];

/// `errors::Error::with_field(key, value) -> errors::Error` - a copy of
/// `err` carrying one more structured diagnostic field. Re-setting an
/// existing key replaces its value in place, so field order is insertion
/// order. Errors are immutable: the receiver is unchanged.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_error_with_field(
    err: *const GosError,
    key: *const c_char,
    value: *const c_char,
) -> *mut GosError {
    ffi_entry!(std::ptr::null_mut(), {
        let key = unsafe { cstr_owned(key) };
        let value = unsafe { cstr_owned(value) };
        let (message, cause, mut fields) = if err.is_null() {
            (Vec::new(), std::ptr::null_mut(), Vec::new())
        } else {
            let e = unsafe { &*err };
            let msg = if e.message.as_ptr().is_null() {
                Vec::new()
            } else {
                unsafe { crate::c_abi::gos_str_arg_bytes(e.message.as_ptr()) }.to_vec()
            };
            (msg, e.cause.as_ptr(), e.fields.clone())
        };
        match fields.iter_mut().find(|(name, _)| *name == key) {
            Some((_, current)) => *current = value,
            None => fields.push((key, value)),
        }
        Box::into_raw(Box::new(GosError {
            message: SyncRawPtr::new(alloc_cstring(&message)),
            cause: SyncRawPtr::new(cause),
            fields,
        }))
    })
}

/// `errors::Error::field(key) -> Option<String>` - the value attached
/// under `key` on this error, ignoring the cause chain.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_error_field(err: *const GosError, key: *const c_char) -> i128 {
    ffi_entry!(unsafe { crate::c_abi::vec::gos_rt_result_new(1, 0) }, {
        if err.is_null() {
            return unsafe { crate::c_abi::vec::gos_rt_result_new(1, 0) };
        }
        let key = unsafe { cstr_owned(key) };
        match unsafe { &*err }.fields.iter().find(|(n, _)| *n == key) {
            Some((_, value)) => unsafe {
                crate::c_abi::vec::gos_rt_result_new(0, alloc_cstring(value.as_bytes()) as i64)
            },
            None => unsafe { crate::c_abi::vec::gos_rt_result_new(1, 0) },
        }
    })
}

/// `errors::Error::fields() -> Vec<(String, String)>` - the structured
/// diagnostic fields of this error in insertion order.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_error_fields(err: *const GosError) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { crate::c_abi::vec::gos_rt_vec_with_capacity(16, 0) };
        if !err.is_null() {
            for (key, value) in &unsafe { &*err }.fields {
                let pair: [i64; 2] = [
                    alloc_cstring(key.as_bytes()) as i64,
                    alloc_cstring(value.as_bytes()) as i64,
                ];
                unsafe { crate::c_abi::vec::gos_rt_vec_push(out, pair.as_ptr().cast::<u8>()) };
            }
        }
        crate::c_abi::vec::vec_set_slot_children(out, &ERROR_FIELDS_SLOT_CHILDREN);
        out
    })
}

/// `errors::Error::chain() -> Vec<errors::Error>` - this error followed
/// by every ancestor cause, outermost first.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_error_chain(err: *const GosError) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { crate::c_abi::vec::gos_rt_vec_with_capacity(8, 0) };
        let mut cur = err;
        while !cur.is_null() {
            let slot = cur as i64;
            unsafe {
                crate::c_abi::vec::gos_rt_vec_push(out, std::ptr::addr_of!(slot).cast::<u8>());
            }
            cur = unsafe { (*cur).cause.as_ptr() };
        }
        out
    })
}

/// `errors::is(err, sentinel) -> bool` with a sentinel *value* rather
/// than a message: true when `sentinel` is `err` itself or any link of
/// its cause chain. Identity is the error's own handle, so two errors
/// built from the same text stay distinguishable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_error_is_sentinel(
    err: *const GosError,
    sentinel: *const GosError,
) -> i64 {
    ffi_entry!(0, {
        if err.is_null() || sentinel.is_null() {
            return 0;
        }
        let mut cur = err;
        while !cur.is_null() {
            if std::ptr::eq(cur, sentinel) {
                return 1;
            }
            cur = unsafe { (*cur).cause.as_ptr() };
        }
        0
    })
}

/// Owned copy of a nul-terminated C string; the empty string for null.
unsafe fn cstr_owned(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { crate::c_abi::gos_str_arg_string(p) }
}

#[cfg(test)]
mod tests {
    /// The C ABI receives a Gossamer string, whose length header sits before
    /// the pointer. A bare `c"..."` literal has no header, so probing for one
    /// reads outside the literal.
    fn gos_str(text: &str) -> *const c_char {
        crate::c_abi::string::alloc_cstring(text.as_bytes()).cast_const()
    }

    use super::*;
    use std::ffi::CStr;

    fn render(f: unsafe extern "C" fn(*const GosError) -> *mut c_char, e: *mut GosError) -> String {
        let p = unsafe { f(e) };
        let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        unsafe { crate::c_abi::string::gos_rt_str_free(p) };
        s
    }

    #[test]
    fn error_new_displays_message_only() {
        let e = unsafe { gos_rt_error_new(gos_str("boom")) };
        assert_eq!(render(gos_rt_error_display, e), "boom");
        assert_eq!(render(gos_rt_error_message, e), "boom");
    }

    #[test]
    fn wrap_two_deep_displays_colon_joined_chain() {
        let root = unsafe { gos_rt_error_new(gos_str("root")) };
        let mid = unsafe { gos_rt_error_wrap(root, gos_str("mid")) };
        let outer = unsafe { gos_rt_error_wrap(mid, gos_str("outer")) };
        assert_eq!(render(gos_rt_error_display, outer), "outer: mid: root");
    }

    #[test]
    fn wrap_keeps_message_top_level_only() {
        let root = unsafe { gos_rt_error_new(gos_str("root")) };
        let outer = unsafe { gos_rt_error_wrap(root, gos_str("outer")) };
        assert_eq!(render(gos_rt_error_message, outer), "outer");
    }
}
