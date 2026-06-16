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

/// Packs an `Err(errors::Error)` result carrying `msg`.
unsafe fn strconv_err(msg: &str) -> i128 {
    let cs = std::ffi::CString::new(msg).unwrap_or_default();
    let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
    unsafe { gos_rt_result_new(1, err as i64) }
}

/// `strconv::parse_i64_radix(s, base) -> Result<i64, errors::Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_strconv_parse_i64_radix(s: *const c_char, base: i64) -> i128 {
    ffi_entry!(0i128, {
        if s.is_null() {
            return unsafe { strconv_err("parse: null input") };
        }
        let text = unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }.trim();
        let Ok(radix) = u32::try_from(base) else {
            return unsafe { strconv_err(&format!("invalid base {base}")) };
        };
        if !(2..=36).contains(&radix) {
            return unsafe { strconv_err(&format!("invalid base {base}")) };
        }
        match i64::from_str_radix(text, radix) {
            Ok(n) => unsafe { gos_rt_result_new(0, n) },
            Err(_) => unsafe { strconv_err(&format!("invalid integer {text:?} in base {radix}")) },
        }
    })
}

/// `strconv::format_i64_radix(n, base) -> String`. Out-of-range bases fall
/// back to decimal; digits a-z are lowercase.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_strconv_format_i64_radix(n: i64, base: i64) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let radix = u32::try_from(base).unwrap_or(10);
        let out = if !(2..=36).contains(&radix) || n == 0 {
            if n == 0 {
                "0".to_string()
            } else {
                n.to_string()
            }
        } else {
            let negative = n < 0;
            let mut v = i128::from(n).unsigned_abs();
            let r = u128::from(radix);
            let mut digits = Vec::new();
            while v > 0 {
                let d = (v % r) as u32;
                digits.push(std::char::from_digit(d, radix).unwrap_or('0'));
                v /= r;
            }
            if negative {
                digits.push('-');
            }
            digits.iter().rev().collect()
        };
        alloc_cstring(out.as_bytes())
    })
}

/// `strconv::quote(s) -> String` — double-quotes `s`, escaping `"`, `\`, and
/// control characters so [`gos_rt_strconv_unquote`] reverses it exactly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_strconv_quote(s: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let text = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        let mut out = String::with_capacity(text.len() + 2);
        out.push('"');
        for c in text.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                c if c.is_control() => out.push_str(&format!("\\u{{{:04x}}}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        alloc_cstring(out.as_bytes())
    })
}

/// `strconv::unquote(s) -> Result<String, errors::Error>` — reverses
/// [`gos_rt_strconv_quote`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_strconv_unquote(s: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if s.is_null() {
            return unsafe { strconv_err("unquote: null input") };
        }
        let text = unsafe { CStr::from_ptr(s).to_str().unwrap_or("") };
        if text.len() < 2 || !text.starts_with('"') || !text.ends_with('"') {
            return unsafe { strconv_err(&format!("unquote: not a quoted string {text:?}")) };
        }
        let inner = &text[1..text.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        let mut bad = false;
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('u') => {
                    if chars.next() != Some('{') {
                        bad = true;
                        break;
                    }
                    let mut hex = String::new();
                    for hc in chars.by_ref() {
                        if hc == '}' {
                            break;
                        }
                        hex.push(hc);
                    }
                    match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        Some(ch) => out.push(ch),
                        None => {
                            bad = true;
                            break;
                        }
                    }
                }
                _ => {
                    bad = true;
                    break;
                }
            }
        }
        if bad {
            return unsafe { strconv_err(&format!("unquote: bad escape in {text:?}")) };
        }
        let ptr = alloc_cstring(out.as_bytes());
        unsafe { gos_rt_result_new(0, ptr as i64) }
    })
}
