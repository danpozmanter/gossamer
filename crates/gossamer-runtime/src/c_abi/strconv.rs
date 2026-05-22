#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::single_match_else)]
#![allow(clippy::uninlined_format_args)]

use std::ffi::CStr;
use std::os::raw::c_char;

use super::*;

/// `strconv::parse_i64(s) -> Result<i64, errors::Error>`.
/// Result is `*mut GosResult` (disc=0 Ok, disc=1 Err); the Ok
/// payload is the parsed integer, the Err payload is a
/// `*mut GosError` describing the parse failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_strconv_parse_i64(s: *const c_char) -> i128 {
    unsafe { gos_rt_parse_i64_result(s) }
}

/// `strconv::atoi(s) -> Result<i64, errors::Error>` — alias for
/// `parse_i64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_strconv_atoi(s: *const c_char) -> i128 {
    unsafe { gos_rt_parse_i64_result(s) }
}

/// `strconv::parse_f64(s) -> Result<f64, errors::Error>`.
/// Ok payload is the bit-pattern of the parsed `f64`
/// (`f64::to_bits` as i64); the LLVM lowering reads the i64
/// register and reinterprets as `f64` via `bitcast`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_strconv_parse_f64(s: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if s.is_null() {
            let cs = std::ffi::CString::new("parse: null input").unwrap();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let text = unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }.trim();
        if let Ok(x) = text.parse::<f64>() {
            unsafe { gos_rt_result_new(0, x.to_bits() as i64) }
        } else {
            let msg = format!(
                "unexpected byte 0x{:x} at 1:1",
                text.as_bytes().first().copied().unwrap_or(0)
            );
            let cs = std::ffi::CString::new(msg).unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            unsafe { gos_rt_result_new(1, err as i64) }
        }
    })
}

/// `strconv::parse_bool(s) -> Result<bool, errors::Error>`.
/// Accepts `"true"` / `"false"` / `"1"` / `"0"` / `"yes"` /
/// `"no"` (case-insensitive); anything else is an Err.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_strconv_parse_bool(s: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if s.is_null() {
            let cs = std::ffi::CString::new("parse: null input").unwrap();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let text = unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
            .trim()
            .to_ascii_lowercase();
        let b = match text.as_str() {
            "true" | "1" | "yes" | "y" | "on" => Some(true),
            "false" | "0" | "no" | "n" | "off" => Some(false),
            _ => None,
        };
        match b {
            Some(v) => unsafe { gos_rt_result_new(0, i64::from(v)) },
            None => {
                let msg = format!("invalid bool literal {:?}", text);
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
        }
    })
}

/// `strconv::format_i64(n) -> String` — alias for `i64_to_str`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_strconv_format_i64(n: i64) -> *mut c_char {
    unsafe { gos_rt_i64_to_str(n) }
}

/// `strconv::itoa(n) -> String` — alias for `format_i64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_strconv_itoa(n: i64) -> *mut c_char {
    unsafe { gos_rt_i64_to_str(n) }
}

/// `strconv::format_f64(x) -> String` — alias for `f64_to_str`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_strconv_format_f64(x: f64) -> *mut c_char {
    unsafe { gos_rt_f64_to_str(x) }
}

/// `strconv::format_bool(b) -> String` — alias for `bool_to_str`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_strconv_format_bool(b: i32) -> *mut c_char {
    unsafe { gos_rt_bool_to_str(b) }
}
