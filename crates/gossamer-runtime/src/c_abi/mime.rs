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
// mime — RFC 2045 media type parsing + extension lookup.
// All inputs are c-strings; outputs are c-strings or i64 booleans.
// ---------------------------------------------------------------

fn mime_str(p: *const c_char) -> &'static str {
    if p.is_null() {
        return "";
    }
    // Safety: the c-string is borrowed for the duration of this call;
    // the &str lifetime is laundered via transmute. The lifetime
    // 'static is acceptable here because the resulting String never
    // outlives the call frame — alloc_cstring copies before return.
    let s = unsafe { CStr::from_ptr(p).to_str().unwrap_or("") };
    unsafe { std::mem::transmute::<&str, &'static str>(s) }
}

fn mime_parse(s: &str) -> Option<::mime::Mime> {
    s.parse::<::mime::Mime>().ok()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mime_parse(s: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let out = match mime_parse(mime_str(s)) {
            Some(m) => format!("{}/{}", m.type_(), m.subtype()),
            None => String::new(),
        };
        alloc_cstring(out.as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mime_top(s: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let out = mime_parse(mime_str(s))
            .map(|m| m.type_().to_string())
            .unwrap_or_default();
        alloc_cstring(out.as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mime_sub(s: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let out = mime_parse(mime_str(s))
            .map(|m| m.subtype().to_string())
            .unwrap_or_default();
        alloc_cstring(out.as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mime_charset(s: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let out = mime_parse(mime_str(s))
            .and_then(|m| m.get_param("charset").map(|v| v.to_string()))
            .unwrap_or_default();
        alloc_cstring(out.as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mime_boundary(s: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let out = mime_parse(mime_str(s))
            .and_then(|m| m.get_param("boundary").map(|v| v.to_string()))
            .unwrap_or_default();
        alloc_cstring(out.as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mime_param(s: *const c_char, key: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let k = mime_str(key);
        let out = mime_parse(mime_str(s))
            .and_then(|m| m.get_param(k).map(|v| v.to_string()))
            .unwrap_or_default();
        alloc_cstring(out.as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mime_type_by_extension(ext: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let raw = mime_str(ext);
        let trimmed = raw.strip_prefix('.').unwrap_or(raw);
        let out = if trimmed.is_empty() {
            String::new()
        } else {
            mime_guess::from_ext(trimmed)
                .first()
                .map(|m| m.essence_str().to_string())
                .unwrap_or_default()
        };
        alloc_cstring(out.as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mime_extension_by_type(t: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let s = mime_str(t);
        let out = match mime_parse(s) {
            Some(m) => {
                let essence = format!("{}/{}", m.type_(), m.subtype());
                mime_guess::get_mime_extensions_str(&essence)
                    .and_then(|exts| exts.first())
                    .map(ToString::to_string)
                    .unwrap_or_default()
            }
            None => String::new(),
        };
        alloc_cstring(out.as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mime_is_valid(s: *const c_char) -> i64 {
    ffi_entry!(0, { i64::from(mime_parse(mime_str(s)).is_some()) })
}
