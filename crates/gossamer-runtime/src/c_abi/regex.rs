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
// regex module — wraps the host `regex` crate with a c-ABI shim.
// Patterns compile lazily via `gos_rt_regex_compile`; matches /
// captures / replacements operate on `*const Regex` handles
// returned to user code as opaque `*mut GosRegex`.
// ---------------------------------------------------------------

#[repr(transparent)]
pub struct GosRegex {
    inner: ::regex::Regex,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_regex_compile(pat: *const c_char) -> *mut GosRegex {
    ffi_entry!(std::ptr::null_mut(), {
        if pat.is_null() {
            return std::ptr::null_mut();
        }
        let s = unsafe { CStr::from_ptr(pat).to_str() }.unwrap_or("");
        match ::regex::Regex::new(s) {
            Ok(re) => Box::into_raw(Box::new(GosRegex { inner: re })),
            Err(_) => std::ptr::null_mut(),
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_regex_is_match(re: *const GosRegex, text: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if re.is_null() || text.is_null() {
            return 0;
        }
        let s = unsafe { CStr::from_ptr(text).to_str() }.unwrap_or("");
        i64::from(unsafe { (*re).inner.is_match(s) })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_regex_find(
    re: *const GosRegex,
    text: *const c_char,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if re.is_null() || text.is_null() {
            return alloc_cstring(b"");
        }
        let s = unsafe { CStr::from_ptr(text).to_str() }.unwrap_or("");
        match unsafe { (*re).inner.find(s) } {
            Some(m) => alloc_cstring(m.as_str().as_bytes()),
            None => alloc_cstring(b""),
        }
    })
}

/// Returns `Option<(start, end, text)>` as a `*mut GosResult`.
/// disc=0 → Some, disc=1 → None. The payload is a heap-allocated
/// `{start: i64, end: i64, text: *mut c_char}` triple.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_regex_find_opt(
    re: *const GosRegex,
    text: *const c_char,
) -> *mut GosResult {
    ffi_entry!(std::ptr::null_mut(), {
        if re.is_null() || text.is_null() {
            return gos_rt_result_new(1, 0);
        }
        let s = unsafe { CStr::from_ptr(text).to_str() }.unwrap_or("");
        match unsafe { (*re).inner.find(s) } {
            None => gos_rt_result_new(1, 0),
            Some(m) => {
                #[repr(C)]
                struct Triple {
                    start: i64,
                    end: i64,
                    text: i64,
                }
                let cstr = alloc_cstring(m.as_str().as_bytes());
                let triple = Box::into_raw(Box::new(Triple {
                    start: m.start() as i64,
                    end: m.end() as i64,
                    text: cstr as i64,
                }));
                gos_rt_result_new(0, triple as i64)
            }
        }
    })
}

/// Returns `Option<Vec<String>>` as a `*mut GosResult`.
/// disc=0 → Some(captures), disc=1 → None. Group 0 = full match.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_regex_captures(
    re: *const GosRegex,
    text: *const c_char,
) -> *mut GosResult {
    ffi_entry!(std::ptr::null_mut(), {
        if re.is_null() || text.is_null() {
            return gos_rt_result_new(1, 0);
        }
        let s = unsafe { CStr::from_ptr(text).to_str() }.unwrap_or("");
        match unsafe { (*re).inner.captures(s) } {
            None => gos_rt_result_new(1, 0),
            Some(caps) => {
                let inner = unsafe { gos_rt_vec_new(8) };
                for i in 0..caps.len() {
                    let ptr_val: i64 = match caps.get(i) {
                        Some(m) => alloc_cstring(m.as_str().as_bytes()) as i64,
                        None => 0,
                    };
                    unsafe { gos_rt_vec_push(inner, std::ptr::addr_of!(ptr_val).cast::<u8>()) };
                }
                gos_rt_result_new(0, inner as i64)
            }
        }
    })
}

/// Finds every non-overlapping match of `re` in `text` and returns
/// a `Vec<String>` of the matched substrings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_regex_find_all(
    re: *const GosRegex,
    text: *const c_char,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        // Each element is a 24-byte `(i64 start, i64 end, *c_char text)`
        // tuple. The previous 8-byte-per-element shape only stored the
        // matched text, leaving `hit.0` / `hit.1` reading garbage and
        // `hit.2` indexing past the end of the buffer (which the
        // example then printed as an empty string).
        let vec = unsafe { gos_rt_vec_new(24) };
        if re.is_null() || text.is_null() {
            return vec;
        }
        let s = unsafe { CStr::from_ptr(text).to_str() }.unwrap_or("");
        for m in unsafe { (*re).inner.find_iter(s) } {
            let cstr = alloc_cstring(m.as_str().as_bytes());
            #[repr(C)]
            struct Tup {
                start: i64,
                end: i64,
                text: i64,
            }
            let entry = Tup {
                start: m.start() as i64,
                end: m.end() as i64,
                text: cstr as i64,
            };
            unsafe {
                gos_rt_vec_push(vec, std::ptr::addr_of!(entry).cast::<u8>());
            }
        }
        vec
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_regex_replace_all(
    re: *const GosRegex,
    text: *const c_char,
    repl: *const c_char,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if re.is_null() || text.is_null() {
            return alloc_cstring(b"");
        }
        let s = unsafe { CStr::from_ptr(text).to_str() }.unwrap_or("");
        let r = if repl.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(repl).to_str() }.unwrap_or("")
        };
        alloc_cstring(unsafe { (*re).inner.replace_all(s, r) }.as_bytes())
    })
}

/// Replaces only the first match of `re` in `text` with `repl`.
/// Companion to [`gos_rt_regex_replace_all`] — separate symbol so
/// the codegen dispatch tables can pick the right semantics.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_regex_replace(
    re: *const GosRegex,
    text: *const c_char,
    repl: *const c_char,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if re.is_null() || text.is_null() {
            return alloc_cstring(b"");
        }
        let s = unsafe { CStr::from_ptr(text).to_str() }.unwrap_or("");
        let r = if repl.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(repl).to_str() }.unwrap_or("")
        };
        alloc_cstring(unsafe { (*re).inner.replace(s, r) }.as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_regex_split(
    re: *const GosRegex,
    text: *const c_char,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let vec = unsafe { gos_rt_vec_new(8) };
        if re.is_null() || text.is_null() {
            return vec;
        }
        let s = unsafe { CStr::from_ptr(text).to_str() }.unwrap_or("");
        for piece in unsafe { (*re).inner.split(s) } {
            let cstr = alloc_cstring(piece.as_bytes());
            let ptr_val = cstr as i64;
            unsafe {
                gos_rt_vec_push(vec, std::ptr::addr_of!(ptr_val).cast::<u8>());
            }
        }
        vec
    })
}

/// Returns `Vec<Vec<*c_char>>` — outer Vec has one entry per
/// match, inner Vec has one entry per group (group 0 = full
/// match, group 1+ = sub-captures). Missing groups are NULL
/// (which user code can pattern-match as `Option::None` because
/// the runtime treats null pointers as the zero discriminant).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_regex_captures_all(
    re: *const GosRegex,
    text: *const c_char,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let outer = unsafe { gos_rt_vec_new(8) };
        if re.is_null() || text.is_null() {
            return outer;
        }
        let s = unsafe { CStr::from_ptr(text).to_str() }.unwrap_or("");
        for caps in unsafe { (*re).inner.captures_iter(s) } {
            let inner = unsafe { gos_rt_vec_new(8) };
            for i in 0..caps.len() {
                let ptr_val: i64 = match caps.get(i) {
                    Some(m) => alloc_cstring(m.as_str().as_bytes()) as i64,
                    None => 0,
                };
                unsafe {
                    gos_rt_vec_push(inner, std::ptr::addr_of!(ptr_val).cast::<u8>());
                }
            }
            let inner_val = inner as i64;
            unsafe {
                gos_rt_vec_push(outer, std::ptr::addr_of!(inner_val).cast::<u8>());
            }
        }
        outer
    })
}
