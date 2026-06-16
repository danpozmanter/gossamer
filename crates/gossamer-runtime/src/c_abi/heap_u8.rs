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
// Heap [u8] primitive (`U8Vec`)
// ---------------------------------------------------------------
//
// Mirrors `GosI64Vec` but stores one byte per element. The
// motivating use case is fasta-style scratch buffers where each
// element is a single ASCII character - using `i64` storage
// blew memory up by 8x with no upside since the workers only
// ever write 0..=255.

#[repr(C)]
pub struct GosU8Vec {
    /// Length in elements (= bytes).
    pub len: i64,
    /// Heap-allocated backing storage. `len` bytes, 1-byte
    /// aligned.
    pub data: *mut u8,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_u8_new(len: i64) -> *mut GosU8Vec {
    ffi_entry!(std::ptr::null_mut(), {
        if len < 0 {
            return std::ptr::null_mut();
        }
        let n = len as usize;
        let mut v: Vec<u8> = vec![0u8; n];
        let data = v.as_mut_ptr();
        std::mem::forget(v);
        Box::into_raw(Box::new(GosU8Vec { len, data }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_u8_free(v: *mut GosU8Vec) {
    ffi_entry!((), {
        if v.is_null() {
            return;
        }
        let v = unsafe { Box::from_raw(v) };
        if !v.data.is_null() {
            let n = v.len as usize;
            unsafe {
                let _ = Vec::from_raw_parts(v.data, n, n);
            }
        }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_u8_get(v: *const GosU8Vec, idx: i64) -> i64 {
    ffi_entry!(-1, {
        if v.is_null() || idx < 0 {
            return 0;
        }
        let v = unsafe { &*v };
        if idx >= v.len || v.data.is_null() {
            return 0;
        }
        unsafe { i64::from(*v.data.add(idx as usize)) }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_u8_set(v: *mut GosU8Vec, idx: i64, val: i64) {
    ffi_entry!((), {
        if v.is_null() || idx < 0 {
            return;
        }
        let v_ref = unsafe { &*v };
        if idx >= v_ref.len || v_ref.data.is_null() {
            return;
        }
        // Truncate to a byte; callers pass `i64`-typed source values
        // that always live in `0..=255` for this use case.
        unsafe { *v_ref.data.add(idx as usize) = val as u8 };
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_u8_len(v: *const GosU8Vec) -> i64 {
    ffi_entry!(-1, {
        if v.is_null() {
            return 0;
        }
        unsafe { (*v).len }
    })
}

/// Materialises the first `len` bytes of a `U8Vec` into a fresh
/// immutable `String` (NUL-terminated arena allocation). The
/// canonical "freeze the build buffer" step at the end of an
/// incremental construction loop - equivalent to F#'s
/// `StringBuilder.ToString()` or Rust's
/// `String::from_utf8(vec).unwrap()`.
///
/// `len` is a separate argument because callers typically
/// pre-allocate a capacity-sized `U8Vec` and write fewer bytes
/// than the buffer's nominal length. Returns the empty string
/// when `v` is null, `len` is non-positive, or `len` exceeds the
/// buffer's nominal length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_u8_to_string(v: *const GosU8Vec, len: i64) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if v.is_null() || len <= 0 {
            return alloc_cstring(b"");
        }
        let v_ref = unsafe { &*v };
        if v_ref.data.is_null() {
            return alloc_cstring(b"");
        }
        let cap = v_ref.len.max(0) as usize;
        let take = (len as usize).min(cap);
        let bytes: &[u8] = unsafe { std::slice::from_raw_parts(v_ref.data, take) };
        alloc_cstring(bytes)
    })
}

/// Bulk write `v[start..start+count]` to stdout, emitting a
/// newline after every `line_width` bytes. Single FFI call so
/// fasta-shape programs don't pay one `gos_rt_print_*` per byte.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_u8_write_lines_to_stdout(
    v: *const GosU8Vec,
    start: i64,
    count: i64,
    line_width: i64,
) {
    ffi_entry!((), {
        if v.is_null() || start < 0 || count <= 0 || line_width <= 0 {
            return;
        }
        let v_ref = unsafe { &*v };
        if v_ref.data.is_null() {
            return;
        }
        let end = start.saturating_add(count);
        if end > v_ref.len {
            return;
        }
        let _guard = StdoutGuard::acquire();
        let bytes_ptr = GOS_RT_STDOUT_BYTES.0.get();
        let len_ptr = GOS_RT_STDOUT_LEN.0.get();
        let mut cur = unsafe { *len_ptr };
        let mut col: i64 = 0;
        let mut idx = start as usize;
        let end = (start + count) as usize;
        while idx < end {
            if cur >= STDOUT_BUF_SIZE {
                unsafe {
                    raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), cur));
                }
                cur = 0;
            }
            let avail = STDOUT_BUF_SIZE - cur;
            let chars_to_eol = (line_width - col) as usize;
            let chars_left = end - idx;
            let take = std::cmp::min(chars_to_eol, std::cmp::min(chars_left, avail));
            // u8 → u8 plain memcpy.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    v_ref.data.add(idx),
                    (*bytes_ptr).as_mut_ptr().add(cur),
                    take,
                );
            }
            cur += take;
            idx += take;
            col += take as i64;
            if col >= line_width {
                if cur >= STDOUT_BUF_SIZE {
                    unsafe {
                        raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), cur));
                    }
                    cur = 0;
                }
                unsafe {
                    *(*bytes_ptr).as_mut_ptr().add(cur) = b'\n';
                }
                cur += 1;
                col = 0;
            }
        }
        if col > 0 {
            if cur >= STDOUT_BUF_SIZE {
                unsafe {
                    raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), cur));
                }
                cur = 0;
            }
            unsafe {
                *(*bytes_ptr).as_mut_ptr().add(cur) = b'\n';
            }
            cur += 1;
        }
        unsafe { *len_ptr = cur };
    });
}

/// Bulk-write the bytes of `v[start..start+count]` to stdout,
/// no line breaks. Used by the phased fasta variant where one
/// "phase" fills the buffer with whole 60-byte lines (newlines
/// already in the buffer) and then dumps it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_u8_write_bytes_to_stdout(
    v: *const GosU8Vec,
    start: i64,
    count: i64,
) {
    ffi_entry!((), {
        if v.is_null() || start < 0 || count <= 0 {
            return;
        }
        let v_ref = unsafe { &*v };
        if v_ref.data.is_null() {
            return;
        }
        let end = start.saturating_add(count);
        if end > v_ref.len {
            return;
        }
        let _guard = StdoutGuard::acquire();
        let bytes_ptr = GOS_RT_STDOUT_BYTES.0.get();
        let len_ptr = GOS_RT_STDOUT_LEN.0.get();
        let mut cur = unsafe { *len_ptr };
        let n = count as usize;
        let mut idx = start as usize;
        let mut written = 0usize;
        while written < n {
            if cur >= STDOUT_BUF_SIZE {
                unsafe {
                    raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), cur));
                }
                cur = 0;
            }
            let avail = STDOUT_BUF_SIZE - cur;
            let take = std::cmp::min(avail, n - written);
            unsafe {
                std::ptr::copy_nonoverlapping(
                    v_ref.data.add(idx),
                    (*bytes_ptr).as_mut_ptr().add(cur),
                    take,
                );
            }
            cur += take;
            idx += take;
            written += take;
            if cur == STDOUT_BUF_SIZE {
                unsafe {
                    raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), cur));
                }
                cur = 0;
            }
        }
        unsafe { *len_ptr = cur };
    });
}
