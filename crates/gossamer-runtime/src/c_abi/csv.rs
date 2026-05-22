#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]

//! `std::encoding::csv` C-ABI shims. Mirrors
//! `gossamer_std::encoding::csv` exactly. CSV records cross the ABI
//! as `Vec<Vec<String>>` — an outer `GosVec` of inner `GosVec`
//! pointers, each inner holding c-string pointers.

use std::ffi::CStr;
use std::os::raw::c_char;

use super::errors::gos_rt_error_new;
use super::string::alloc_cstring;
use super::vec::{GosVec, gos_rt_result_new, gos_rt_vec_push, gos_rt_vec_with_capacity};

/// Reads a `GosVec<String>` (elements are c-string pointers) into
/// owned strings.
unsafe fn read_str_vec(v: *const GosVec) -> Vec<String> {
    if v.is_null() {
        return Vec::new();
    }
    let vref = unsafe { &*v };
    if vref.ptr.is_null() || vref.len <= 0 {
        return Vec::new();
    }
    let len = vref.len as usize;
    let words = unsafe { std::slice::from_raw_parts(vref.ptr.as_ptr().cast::<i64>(), len) };
    words
        .iter()
        .map(|&w| {
            let p = w as *const c_char;
            if p.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
            }
        })
        .collect()
}

/// Builds a `GosVec<String>` from owned strings.
fn build_str_vec(parts: &[String]) -> *mut GosVec {
    let vec = unsafe { gos_rt_vec_with_capacity(8, parts.len() as i64) };
    for p in parts {
        let pv = alloc_cstring(p.as_bytes()) as i64;
        unsafe { gos_rt_vec_push(vec, std::ptr::addr_of!(pv).cast::<u8>()) };
    }
    vec
}

fn parse_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes => {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            }
            '"' => in_quotes = true,
            ',' if !in_quotes => {
                fields.push(std::mem::take(&mut field));
            }
            _ => field.push(c),
        }
    }
    fields.push(field);
    fields
}

unsafe fn cstr<'a>(p: *const c_char) -> &'a str {
    if p.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(p) }.to_str().unwrap_or("")
    }
}

/// `encoding::csv::parse_line(line) -> [String]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_csv_parse_line(line: *const c_char) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        build_str_vec(&parse_line(unsafe { cstr(line) }))
    })
}

/// `encoding::csv::read(input) -> Result<[[String]], Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_csv_read(input: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let input = unsafe { cstr(input) };
        let mut rows: Vec<Vec<String>> = Vec::new();
        for line in input.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let quote_count = line.chars().filter(|&c| c == '"').count();
            if quote_count % 2 != 0 {
                let msg = format!("csv: unterminated quoted field in: {line}");
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                return unsafe { gos_rt_result_new(1, err as i64) };
            }
            rows.push(parse_line(line));
        }
        // Build the outer GosVec of inner GosVec<String> pointers.
        let outer = unsafe { gos_rt_vec_with_capacity(8, rows.len() as i64) };
        for row in &rows {
            let inner = build_str_vec(row) as i64;
            unsafe { gos_rt_vec_push(outer, std::ptr::addr_of!(inner).cast::<u8>()) };
        }
        unsafe { gos_rt_result_new(0, outer as i64) }
    })
}

/// `encoding::csv::write(records) -> String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_csv_write(records: *const GosVec) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let rows: Vec<Vec<String>> = if records.is_null() {
            Vec::new()
        } else {
            let vref = unsafe { &*records };
            if vref.ptr.is_null() || vref.len <= 0 {
                Vec::new()
            } else {
                let len = vref.len as usize;
                let words =
                    unsafe { std::slice::from_raw_parts(vref.ptr.as_ptr().cast::<i64>(), len) };
                words
                    .iter()
                    .map(|&w| unsafe { read_str_vec(w as *const GosVec) })
                    .collect()
            }
        };
        let mut out = String::new();
        for (i, record) in rows.iter().enumerate() {
            for (j, field) in record.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                if field.contains(',') || field.contains('"') || field.contains('\n') {
                    out.push('"');
                    out.push_str(&field.replace('"', "\"\""));
                    out.push('"');
                } else {
                    out.push_str(field);
                }
            }
            if i + 1 < rows.len() {
                out.push('\n');
            }
        }
        alloc_cstring(out.as_bytes())
    })
}
