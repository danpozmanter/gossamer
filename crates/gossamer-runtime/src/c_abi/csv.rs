#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]

//! `std::encoding::csv` C-ABI shims. Mirrors
//! `gossamer_std::encoding::csv` exactly. CSV records cross the ABI
//! as `Vec<Vec<String>>` - an outer `GosVec` of inner `GosVec`
//! pointers, each inner holding c-string pointers.

use std::ffi::CStr;
use std::os::raw::c_char;

use super::errors::gos_rt_error_new;
use super::string::alloc_cstring;
use super::vec::{GosVec, gos_rt_result_new, gos_rt_vec_push};

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

/// Builds a `GosVec<String>` from owned strings. STRING-typed: the
/// vec owns each element, so `gos_rt_vec_free` deep-frees them.
fn build_str_vec(parts: &[String]) -> *mut GosVec {
    let vec = unsafe {
        crate::c_abi::vec::gos_rt_vec_with_capacity_typed(
            8,
            parts.len() as i64,
            crate::c_abi::vec::vec_elem_kind::STRING,
        )
    };
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
        // VEC-typed: the outer vec owns each row, so `gos_rt_vec_free`
        // cascades through unvisited rows (the early-`break` path)
        // instead of leaking them and their field strings.
        let outer = unsafe {
            crate::c_abi::vec::gos_rt_vec_with_capacity_typed(
                8,
                rows.len() as i64,
                crate::c_abi::vec::vec_elem_kind::VEC,
            )
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Refcount word of an `alloc_cstring` builder-layout string:
    /// `[rc:u32][cap:u32][len:u32][tag][content][NUL]`, body at +13.
    unsafe fn str_rc(s: *const c_char) -> u32 {
        let hdr = unsafe { s.cast::<u8>().sub(13) };
        u32::from_le_bytes(unsafe { [*hdr, *hdr.add(1), *hdr.add(2), *hdr.add(3)] })
    }

    #[test]
    fn csv_read_outer_vec_is_vec_typed_and_deep_frees_unvisited_rows() {
        let input = std::ffi::CString::new("a,b\nc,d\ne,f").unwrap();
        let r = unsafe { gos_rt_csv_read(input.as_ptr()) };
        assert_eq!(crate::c_abi::vec::gos_rt_result_disc(r), 0);
        let outer = crate::c_abi::vec::gos_rt_result_payload(r) as *mut GosVec;
        assert!(!outer.is_null());
        let o = unsafe { &*outer };
        assert_eq!(o.len, 3);
        assert_eq!(o.elem_kind, crate::c_abi::vec::vec_elem_kind::VEC);
        // Probe-share row 1's first field, then free the outer WITHOUT
        // iterating (the ABI shape of `for row in rows { break }`): the
        // cascade must release exactly one share - rc 2 -> 1, not 2
        // (leak) and not 0 (double free).
        // Slots hold child pointers exposed as i64 by the flat-slot ABI;
        // read the address and recover its provenance.
        let row1: *mut GosVec = std::ptr::with_exposed_provenance_mut(unsafe {
            (o.ptr.add(8) as *const usize).read_unaligned()
        });
        let field: *mut c_char = std::ptr::with_exposed_provenance_mut(unsafe {
            ((*row1).ptr.as_ptr() as *const usize).read_unaligned()
        });
        unsafe { crate::c_abi::string::gos_rt_str_retain(field) };
        assert_eq!(unsafe { str_rc(field) }, 2);
        unsafe { crate::c_abi::map::gos_rt_vec_free(outer) };
        assert_eq!(
            unsafe { str_rc(field) },
            1,
            "outer free must cascade exactly once"
        );
        assert_eq!(unsafe { CStr::from_ptr(field) }.to_str().unwrap(), "c");
        unsafe { crate::c_abi::string::gos_rt_str_free(field) };
    }

    #[test]
    fn csv_read_borrow_all_rows_then_free_is_balanced() {
        let input = std::ffi::CString::new("x,y\nz,w").unwrap();
        let r = unsafe { gos_rt_csv_read(input.as_ptr()) };
        assert_eq!(crate::c_abi::vec::gos_rt_result_disc(r), 0);
        let outer = crate::c_abi::vec::gos_rt_result_payload(r) as *mut GosVec;
        let o = unsafe { &*outer };
        // Full-iteration consumer shape: every read is an interior
        // borrow (the drop pass never releases container loads), so a
        // single outer free afterwards is the only release.
        let mut fields = Vec::new();
        for i in 0..o.len as usize {
            // Slots hold child pointers exposed as i64 by the flat-slot
            // ABI; read the address and recover its provenance so the
            // borrow is sound under strict provenance.
            let raw = unsafe { (o.ptr.add(i * 8) as *const usize).read_unaligned() };
            let row: *mut GosVec = std::ptr::with_exposed_provenance_mut(raw);
            let rv = unsafe { &*row };
            for j in 0..rv.len as usize {
                let raw = unsafe { (rv.ptr.add(j * 8) as *const usize).read_unaligned() };
                let f: *mut c_char = std::ptr::with_exposed_provenance_mut(raw);
                fields.push(unsafe { CStr::from_ptr(f) }.to_str().unwrap().to_string());
            }
        }
        assert_eq!(fields, ["x", "y", "z", "w"]);
        unsafe { crate::c_abi::map::gos_rt_vec_free(outer) };
    }
}
