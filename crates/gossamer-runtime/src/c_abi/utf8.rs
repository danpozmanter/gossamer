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

//! C-ABI surface for `std::utf8`. Mirrors the most common helpers
//! exposed by `gossamer_std::utf8` in shapes the compiled tiers
//! can call directly:
//! - bool predicates return `i64` (0/1)
//! - `char` arguments arrive as `u32`
//! - `String` arguments arrive as `*const c_char`
//!
//! The tuple-returning `decode_rune` family is intentionally not
//! re-exported here; those callers stay on the interp tier until
//! the Adt-by-value ABI is wired through. Most production code
//! reaches for `rune_count*` / `rune_len*` / `is_valid` /
//! `full_rune_in_string`, all of which work.

use std::ffi::CStr;
use std::os::raw::c_char;

#[inline]
unsafe fn cstr_to_str<'a>(s: *const c_char) -> &'a str {
    if s.is_null() {
        return "";
    }
    unsafe { CStr::from_ptr(s) }.to_str().unwrap_or("")
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_utf8_rune_count_in_string(s: *const c_char) -> i64 {
    ffi_entry!(0, {
        let text = unsafe { cstr_to_str(s) };
        text.chars().count() as i64
    })
}

/// Convenience alias — `utf8::count_runes(&str)` and
/// `utf8::rune_count(&str)` route here too when the caller passes a
/// string rather than a byte slice. (Byte-slice callers go through
/// [`gos_rt_utf8_rune_count_bytes`].)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_utf8_count_runes(s: *const c_char) -> i64 {
    unsafe { gos_rt_utf8_rune_count_in_string(s) }
}

/// `utf8::rune_count(bytes: *mut GosVec) -> i64` — counts code
/// points in a byte buffer. Invalid sequences return 0 for the run
/// they cover (matching Rust's `std::str::from_utf8` failure mode).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_utf8_rune_count_bytes(
    vec: *const crate::c_abi::vec::GosVec,
) -> i64 {
    ffi_entry!(0, {
        if vec.is_null() {
            return 0;
        }
        let header = unsafe { &*vec };
        if header.ptr.is_null() || header.len <= 0 {
            return 0;
        }
        let slice =
            unsafe { std::slice::from_raw_parts(header.ptr.as_const_ptr(), header.len as usize) };
        std::str::from_utf8(slice).map_or(0, |s| s.chars().count() as i64)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_utf8_rune_len(c: u32) -> i64 {
    ffi_entry!(-1, {
        char::from_u32(c).map_or(-1, |ch| ch.len_utf8() as i64)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_utf8_valid_rune(c: u32) -> i64 {
    ffi_entry!(0, { i64::from(char::from_u32(c).is_some()) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_utf8_valid_string(s: *const c_char) -> i64 {
    ffi_entry!(0, {
        if s.is_null() {
            return 1;
        }
        // CStr::to_str validates UTF-8; reaching the function with a
        // valid cstring is a strong-enough check for the common
        // caller path.
        let ok = unsafe { CStr::from_ptr(s) }.to_str().is_ok();
        i64::from(ok)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_utf8_is_valid(vec: *const crate::c_abi::vec::GosVec) -> i64 {
    ffi_entry!(0, {
        if vec.is_null() {
            return 1;
        }
        let header = unsafe { &*vec };
        if header.ptr.is_null() || header.len <= 0 {
            return 1;
        }
        let slice =
            unsafe { std::slice::from_raw_parts(header.ptr.as_const_ptr(), header.len as usize) };
        i64::from(std::str::from_utf8(slice).is_ok())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_utf8_full_rune_in_string(s: *const c_char) -> i64 {
    ffi_entry!(0, {
        let text = unsafe { cstr_to_str(s) };
        let bytes = text.as_bytes();
        if bytes.is_empty() {
            return 0;
        }
        let first = bytes[0];
        let width = if first < 0x80 {
            1
        } else if first < 0xC0 {
            return 0;
        } else if first < 0xE0 {
            2
        } else if first < 0xF0 {
            3
        } else {
            4
        };
        i64::from(bytes.len() >= width)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_utf8_rune_start(b: u32) -> i64 {
    ffi_entry!(0, { i64::from((b as u8) & 0xC0 != 0x80) })
}
