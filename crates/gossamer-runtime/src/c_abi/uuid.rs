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
// uuid - v4 (random) and v7 (timestamp-ordered) UUID generation,
// parsing, and normalization. Logic lives in the runtime crate
// (compiled tier links against `libgossamer_runtime.a` directly);
// `gossamer-std::uuid` is a thin facade that re-exports these
// functions for the interpreter.
// ---------------------------------------------------------------

/// Generates a fresh v4 (random) UUID and returns the canonical
/// hyphenated form as a heap-owned c-string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_uuid_v4() -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let s = ::uuid::Uuid::new_v4().hyphenated().to_string();
        alloc_cstring(s.as_bytes())
    })
}

/// Generates a fresh v7 (timestamp-ordered) UUID.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_uuid_v7() -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let s = ::uuid::Uuid::now_v7().hyphenated().to_string();
        alloc_cstring(s.as_bytes())
    })
}

/// Returns 1 iff `s` parses as a canonical UUID.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_uuid_is_valid(s: *const c_char) -> i64 {
    ffi_entry!(0, {
        if s.is_null() {
            return 0;
        }
        let s = unsafe { CStr::from_ptr(s).to_str().unwrap_or("") };
        i64::from(::uuid::Uuid::parse_str(s).is_ok())
    })
}

/// Returns the lowercase canonical form of `s` if it parses, else the empty string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_uuid_normalize(s: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if s.is_null() {
            return alloc_cstring(b"");
        }
        let s = unsafe { CStr::from_ptr(s).to_str().unwrap_or("") };
        let out = match ::uuid::Uuid::parse_str(s) {
            Ok(u) => u.hyphenated().to_string(),
            Err(_) => String::new(),
        };
        alloc_cstring(out.as_bytes())
    })
}

/// Returns the 32-char unhyphenated form of `s`, else the empty string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_uuid_simple(s: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if s.is_null() {
            return alloc_cstring(b"");
        }
        let s = unsafe { CStr::from_ptr(s).to_str().unwrap_or("") };
        let out = match ::uuid::Uuid::parse_str(s) {
            Ok(u) => u.simple().to_string(),
            Err(_) => String::new(),
        };
        alloc_cstring(out.as_bytes())
    })
}

// ======================================================================
// std::iter combinators - AOT runtime helpers.
//
// The interp wires these as native fns in stdlib_builtins.rs; this block
// is the cranelift + LLVM counterpart. SPEC §10.4: data-last argument
// order; combinators specialize on i64 element width where it matters
// (the dominant case for benchmark-shaped code), with `_ptr` variants
// for word-sized pointer elements (strings and aggregates).
//
// Closure-taking helpers follow the env-ptr + fn_addr@env[0] ABI
// established by `gos_rt_arr_sort_by_i64` (above). Each helper
// transmutes env[0] to a typed `fn(env, args...) -> ret` pointer and
// calls back through it once per element.

/// Return the element count of `v` as i64 (`iter::count(xs)`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_count(v: *const GosVec) -> i64 {
    ffi_entry!(-1, {
        if v.is_null() {
            return 0;
        }
        unsafe { (*v).len }
    })
}

/// Sum all i64 elements of `v`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_sum_i64(v: *const GosVec) -> i64 {
    ffi_entry!(-1, {
        if v.is_null() {
            return 0;
        }
        let vec = unsafe { &*v };
        if vec.ptr.is_null() || vec.len <= 0 {
            return 0;
        }
        let slice = unsafe { std::slice::from_raw_parts(vec.ptr.cast::<i64>(), vec.len as usize) };
        slice.iter().copied().sum()
    })
}

/// Sum all f64 elements of `v`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_sum_f64(v: *const GosVec) -> f64 {
    ffi_entry!(f64::NAN, {
        if v.is_null() {
            return 0.0;
        }
        let vec = unsafe { &*v };
        if vec.ptr.is_null() || vec.len <= 0 {
            return 0.0;
        }
        let slice = unsafe { std::slice::from_raw_parts(vec.ptr.cast::<f64>(), vec.len as usize) };
        slice.iter().copied().sum()
    })
}

/// Product of all i64 elements of `v`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_product_i64(v: *const GosVec) -> i64 {
    ffi_entry!(-1, {
        if v.is_null() {
            return 1;
        }
        let vec = unsafe { &*v };
        if vec.ptr.is_null() || vec.len <= 0 {
            return 1;
        }
        let slice = unsafe { std::slice::from_raw_parts(vec.ptr.cast::<i64>(), vec.len as usize) };
        slice.iter().copied().fold(1i64, i64::wrapping_mul)
    })
}

/// Product of all f64 elements of `v`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_product_f64(v: *const GosVec) -> f64 {
    ffi_entry!(f64::NAN, {
        if v.is_null() {
            return 1.0;
        }
        let vec = unsafe { &*v };
        if vec.ptr.is_null() || vec.len <= 0 {
            return 1.0;
        }
        let slice = unsafe { std::slice::from_raw_parts(vec.ptr.cast::<f64>(), vec.len as usize) };
        slice.iter().copied().product()
    })
}

/// `iter::min(xs) -> Option<i64>` as an i128-packed Option:
/// `None` (= 1) for empty input, `Some(m)` otherwise. Matches the
/// 16-byte Option ABI the typechecker pins for `iter::min`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_min_i64(v: *const GosVec) -> i128 {
    ffi_entry!(1i128, {
        if v.is_null() {
            return 1i128;
        }
        let vec = unsafe { &*v };
        if vec.ptr.is_null() || vec.len <= 0 {
            return 1i128;
        }
        let slice = unsafe { std::slice::from_raw_parts(vec.ptr.cast::<i64>(), vec.len as usize) };
        match slice.iter().copied().min() {
            Some(m) => gos_rt_result_new(0, m),
            None => 1i128,
        }
    })
}

/// `iter::max(xs) -> Option<i64>` as an i128-packed Option:
/// `None` (= 1) for empty input, `Some(m)` otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_max_i64(v: *const GosVec) -> i128 {
    ffi_entry!(1i128, {
        if v.is_null() {
            return 1i128;
        }
        let vec = unsafe { &*v };
        if vec.ptr.is_null() || vec.len <= 0 {
            return 1i128;
        }
        let slice = unsafe { std::slice::from_raw_parts(vec.ptr.cast::<i64>(), vec.len as usize) };
        match slice.iter().copied().max() {
            Some(m) => gos_rt_result_new(0, m),
            None => 1i128,
        }
    })
}

/// Build a `Vec<i64>` of `[start, end)`. Empty if `end <= start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_range(start: i64, end: i64) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        if end > start {
            for n in start..end {
                unsafe { gos_rt_vec_push_i64(out, n) };
            }
        }
        out
    })
}

/// Build a `Vec<i64>` of `[start, end]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_range_inclusive(start: i64, end: i64) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        if end >= start {
            for n in start..=end {
                unsafe { gos_rt_vec_push_i64(out, n) };
            }
        }
        out
    })
}

/// Build `Vec<i64>` of length `n` filled with `value`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_repeat_i64(value: i64, n: i64) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        if n > 0 {
            for _ in 0..n {
                unsafe { gos_rt_vec_push_i64(out, value) };
            }
        }
        out
    })
}

/// Build `Vec<i64>` from the first `n` elements of `v`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_take_i64(n: i64, v: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        if v.is_null() {
            return out;
        }
        let vec = unsafe { &*v };
        let take_n = n.max(0).min(vec.len);
        for i in 0..take_n {
            let x = unsafe { gos_rt_vec_get_i64(v, i) };
            unsafe { gos_rt_vec_push_i64(out, x) };
        }
        out
    })
}

/// Build `Vec<i64>` dropping the first `n` elements of `v`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_skip_i64(n: i64, v: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        if v.is_null() {
            return out;
        }
        let vec = unsafe { &*v };
        let start = n.max(0).min(vec.len);
        for i in start..vec.len {
            let x = unsafe { gos_rt_vec_get_i64(v, i) };
            unsafe { gos_rt_vec_push_i64(out, x) };
        }
        out
    })
}

/// Reverse a `Vec<i64>` into a fresh vec.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_reversed_i64(v: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        if v.is_null() {
            return out;
        }
        let vec = unsafe { &*v };
        for i in (0..vec.len).rev() {
            let x = unsafe { gos_rt_vec_get_i64(v, i) };
            unsafe { gos_rt_vec_push_i64(out, x) };
        }
        out
    })
}

/// Concatenate two `Vec<i64>`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_chain_i64(a: *const GosVec, b: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        for v in [a, b] {
            if v.is_null() {
                continue;
            }
            let vec = unsafe { &*v };
            for i in 0..vec.len {
                let x = unsafe { gos_rt_vec_get_i64(v, i) };
                unsafe { gos_rt_vec_push_i64(out, x) };
            }
        }
        out
    })
}

/// `iter::dedup(xs)` - drop consecutive duplicate elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_dedup_i64(v: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        if v.is_null() {
            return out;
        }
        let vec = unsafe { &*v };
        let mut prev: Option<i64> = None;
        for i in 0..vec.len {
            let x = unsafe { gos_rt_vec_get_i64(v, i) };
            if prev != Some(x) {
                unsafe { gos_rt_vec_push_i64(out, x) };
                prev = Some(x);
            }
        }
        out
    })
}

/// `iter::flatten(xss)` - concatenate a `Vec<Vec<i64>>` into one
/// `Vec<i64>`. Each outer element is an 8-byte `*mut GosVec`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_flatten_i64(v: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        if v.is_null() {
            return out;
        }
        let outer = unsafe { &*v };
        for i in 0..outer.len {
            let inner = unsafe { gos_rt_vec_get_i64(v, i) } as usize as *const GosVec;
            if inner.is_null() {
                continue;
            }
            let inner_ref = unsafe { &*inner };
            for j in 0..inner_ref.len {
                let x = unsafe { gos_rt_vec_get_i64(inner, j) };
                unsafe { gos_rt_vec_push_i64(out, x) };
            }
        }
        out
    })
}

/// `iter::enumerate(xs)` - `Vec<(i64, i64)>` of `(index, value)`.
/// Each element is a 16-byte 2-slot tuple read by the multislot
/// for-loop path (`gos_rt_vec_get_ptr` + `gos_load` at 0 / 8).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_enumerate_i64(v: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(16) };
        if v.is_null() {
            return out;
        }
        let vec = unsafe { &*v };
        for i in 0..vec.len {
            let x = unsafe { gos_rt_vec_get_i64(v, i) };
            let slot: [i64; 2] = [i, x];
            unsafe { gos_rt_vec_push(out, slot.as_ptr().cast::<u8>()) };
        }
        out
    })
}

/// `iter::zip(a, b)` - `Vec<(i64, i64)>`, stopping at the shorter
/// input. 16-byte 2-slot tuple elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_zip_i64(a: *const GosVec, b: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(16) };
        if a.is_null() || b.is_null() {
            return out;
        }
        let av = unsafe { &*a };
        let bv = unsafe { &*b };
        let n = av.len.min(bv.len);
        for i in 0..n {
            let x = unsafe { gos_rt_vec_get_i64(a, i) };
            let y = unsafe { gos_rt_vec_get_i64(b, i) };
            let slot: [i64; 2] = [x, y];
            unsafe { gos_rt_vec_push(out, slot.as_ptr().cast::<u8>()) };
        }
        out
    })
}

/// `iter::pairwise(xs)` - `Vec<(i64, i64)>` of successive
/// overlapping pairs (width-2 windows).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_pairwise_i64(v: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(16) };
        if v.is_null() {
            return out;
        }
        let vec = unsafe { &*v };
        for i in 1..vec.len {
            let a = unsafe { gos_rt_vec_get_i64(v, i - 1) };
            let b = unsafe { gos_rt_vec_get_i64(v, i) };
            let slot: [i64; 2] = [a, b];
            unsafe { gos_rt_vec_push(out, slot.as_ptr().cast::<u8>()) };
        }
        out
    })
}

/// `iter::windows(n, xs)` - `Vec<Vec<i64>>` of every contiguous
/// width-`n` window. Empty when `n <= 0` or `xs` is shorter than
/// `n`. Outer is a VEC-typed vec of inner `*mut GosVec` pointers
/// (recursively freed).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_windowed_i64(n: i64, v: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe {
            crate::c_abi::vec::gos_rt_vec_new_typed(8, crate::c_abi::vec::vec_elem_kind::VEC)
        };
        if v.is_null() || n <= 0 {
            return out;
        }
        let vec = unsafe { &*v };
        if vec.len < n {
            return out;
        }
        for start in 0..=(vec.len - n) {
            let inner = unsafe { gos_rt_vec_new(8) };
            for j in 0..n {
                let x = unsafe { gos_rt_vec_get_i64(v, start + j) };
                unsafe { gos_rt_vec_push_i64(inner, x) };
            }
            let inner_val = inner as i64;
            unsafe { gos_rt_vec_push(out, std::ptr::addr_of!(inner_val).cast::<u8>()) };
        }
        out
    })
}

/// `iter::chunks(n, xs)` - `Vec<Vec<i64>>` of consecutive
/// width-`n` chunks; the final chunk may be short. Empty when
/// `n <= 0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_chunk_by_size_i64(n: i64, v: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe {
            crate::c_abi::vec::gos_rt_vec_new_typed(8, crate::c_abi::vec::vec_elem_kind::VEC)
        };
        if v.is_null() || n <= 0 {
            return out;
        }
        let vec = unsafe { &*v };
        let mut start = 0;
        while start < vec.len {
            let inner = unsafe { gos_rt_vec_new(8) };
            let end = (start + n).min(vec.len);
            for j in start..end {
                let x = unsafe { gos_rt_vec_get_i64(v, j) };
                unsafe { gos_rt_vec_push_i64(inner, x) };
            }
            let inner_val = inner as i64;
            unsafe { gos_rt_vec_push(out, std::ptr::addr_of!(inner_val).cast::<u8>()) };
            start += n;
        }
        out
    })
}

// -- Closure-taking iter helpers. Closure ABI: env pointer with
// fn_addr at env[0]. Each helper transmutes env[0] to a specific
// `(env, args...) -> ret` signature determined by the combinator's
// callback contract.

/// `iter::for_each(f, xs)` - call `f(x)` once per element.
/// Closure body sig: `(env: *const u8, x: i64) -> i64` (return value
/// ignored; using i64 keeps the callback ABI uniform with sort_by).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_for_each_i64(env: *const u8, v: *const GosVec) {
    ffi_entry!((), {
        if env.is_null() || v.is_null() {
            return;
        }
        let vec = unsafe { &*v };
        if vec.len <= 0 {
            return;
        }
        type CallFn = unsafe extern "C" fn(env: *const u8, x: i64) -> i64;
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return;
        }
        let f: CallFn = unsafe { std::mem::transmute(fn_addr_raw) };
        for i in 0..vec.len {
            let x = unsafe { gos_rt_vec_get_i64(v, i) };
            unsafe { f(env, x) };
        }
    });
}

/// `iter::for_each(f, xs)` for `Vec<String>` / `Vec<*ptr>` shape.
/// Closure body sig: `(env: *const u8, x: *const u8) -> i64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_for_each_ptr(env: *const u8, v: *const GosVec) {
    ffi_entry!((), {
        if env.is_null() || v.is_null() {
            return;
        }
        let vec = unsafe { &*v };
        if vec.len <= 0 {
            return;
        }
        type CallFn = unsafe extern "C" fn(env: *const u8, x: *const u8) -> i64;
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return;
        }
        let f: CallFn = unsafe { std::mem::transmute(fn_addr_raw) };
        for i in 0..vec.len {
            let p = unsafe { gos_rt_vec_get_ptr(v, i) };
            unsafe { f(env, p) };
        }
    });
}

/// `iter::map(f, xs)` for `Vec<i64> -> Vec<i64>`.
/// Closure body sig: `(env, i64) -> i64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_map_i64(env: *const u8, v: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        if env.is_null() || v.is_null() {
            return out;
        }
        let vec = unsafe { &*v };
        type CallFn = unsafe extern "C" fn(env: *const u8, x: i64) -> i64;
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return out;
        }
        let f: CallFn = unsafe { std::mem::transmute(fn_addr_raw) };
        for i in 0..vec.len {
            let x = unsafe { gos_rt_vec_get_i64(v, i) };
            let y = unsafe { f(env, x) };
            unsafe { gos_rt_vec_push_i64(out, y) };
        }
        out
    })
}

/// `iter::filter(p, xs)` for `Vec<i64>`. Predicate returns i64
/// (truthy = nonzero) to keep the callback ABI uniform.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_filter_i64(env: *const u8, v: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        if env.is_null() || v.is_null() {
            return out;
        }
        let vec = unsafe { &*v };
        type PredFn = unsafe extern "C" fn(env: *const u8, x: i64) -> bool;
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return out;
        }
        let p: PredFn = unsafe { std::mem::transmute(fn_addr_raw) };
        for i in 0..vec.len {
            let x = unsafe { gos_rt_vec_get_i64(v, i) };
            if unsafe { p(env, x) } {
                unsafe { gos_rt_vec_push_i64(out, x) };
            }
        }
        // The kept slots are raw copies of the source's; when the source
        // owns pointer-bearing elements the result must hold its own
        // shares (and carry the same element kind) or the source's free
        // would dangle every survivor.
        unsafe { crate::c_abi::vec::vec_share_owned_elements(v, out) };
        out
    })
}

/// `iter::for_each(f, xs)` for `Vec<f64>`. Element bits are read as an
/// 8-byte word and reinterpreted as `f64` so the closure receives the
/// value in the float ABI (an `f64` param rides an SSE register, not the
/// integer register `f(env, x: i64)` would fill).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_for_each_f64(env: *const u8, v: *const GosVec) {
    ffi_entry!((), {
        if env.is_null() || v.is_null() {
            return;
        }
        let vec = unsafe { &*v };
        if vec.len <= 0 {
            return;
        }
        type CallFn = unsafe extern "C" fn(env: *const u8, x: f64) -> i64;
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return;
        }
        let f: CallFn = unsafe { std::mem::transmute(fn_addr_raw) };
        for i in 0..vec.len {
            let x = f64::from_bits(unsafe { gos_rt_vec_get_i64(v, i) } as u64);
            unsafe { f(env, x) };
        }
    });
}

/// `iter::map(f, xs)` for `Vec<f64> -> Vec<f64>`. Reads each element's
/// bits as `f64`, calls the float-ABI closure, and stores the result
/// bits back into the new Vec. The input and output register class each
/// pick their own shim (an `f64` rides an SSE register, an `i64` /
/// pointer an integer one) - a mismatched pairing would read the result
/// out of the wrong register.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_map_f64(env: *const u8, v: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        if env.is_null() || v.is_null() {
            return out;
        }
        let vec = unsafe { &*v };
        type CallFn = unsafe extern "C" fn(env: *const u8, x: f64) -> f64;
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return out;
        }
        let f: CallFn = unsafe { std::mem::transmute(fn_addr_raw) };
        for i in 0..vec.len {
            let x = f64::from_bits(unsafe { gos_rt_vec_get_i64(v, i) } as u64);
            let y = unsafe { f(env, x) };
            unsafe { gos_rt_vec_push_i64(out, y.to_bits() as i64) };
        }
        out
    })
}

/// `iter::map(f, xs)` for `Vec<f64> -> Vec<i64 / ptr>` - an `f64`
/// element mapped to an integer-register result (`|x| x as i64`,
/// `|x| format!("{}", x)`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_map_f64_word(env: *const u8, v: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        if env.is_null() || v.is_null() {
            return out;
        }
        let vec = unsafe { &*v };
        type CallFn = unsafe extern "C" fn(env: *const u8, x: f64) -> i64;
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return out;
        }
        let f: CallFn = unsafe { std::mem::transmute(fn_addr_raw) };
        for i in 0..vec.len {
            let x = f64::from_bits(unsafe { gos_rt_vec_get_i64(v, i) } as u64);
            let y = unsafe { f(env, x) };
            unsafe { gos_rt_vec_push_i64(out, y) };
        }
        out
    })
}

/// `iter::map(f, xs)` for `Vec<i64 / ptr> -> Vec<f64>` - an
/// integer-register element mapped to an `f64` result (`|i| i as f64`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_map_word_f64(env: *const u8, v: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        if env.is_null() || v.is_null() {
            return out;
        }
        let vec = unsafe { &*v };
        type CallFn = unsafe extern "C" fn(env: *const u8, x: i64) -> f64;
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return out;
        }
        let f: CallFn = unsafe { std::mem::transmute(fn_addr_raw) };
        for i in 0..vec.len {
            let x = unsafe { gos_rt_vec_get_i64(v, i) };
            let y = unsafe { f(env, x) };
            unsafe { gos_rt_vec_push_i64(out, y.to_bits() as i64) };
        }
        out
    })
}

/// `iter::filter(p, xs)` for `Vec<f64>`. The kept elements are the
/// original bit patterns; only the predicate sees the reinterpreted
/// `f64` value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_filter_f64(env: *const u8, v: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        if env.is_null() || v.is_null() {
            return out;
        }
        let vec = unsafe { &*v };
        type PredFn = unsafe extern "C" fn(env: *const u8, x: f64) -> bool;
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return out;
        }
        let p: PredFn = unsafe { std::mem::transmute(fn_addr_raw) };
        for i in 0..vec.len {
            let bits = unsafe { gos_rt_vec_get_i64(v, i) };
            if unsafe { p(env, f64::from_bits(bits as u64)) } {
                unsafe { gos_rt_vec_push_i64(out, bits) };
            }
        }
        out
    })
}

/// `iter::fold(init, f, xs)` for `Vec<i64>` with i64 accumulator.
/// Closure body sig: `(env, acc, x) -> acc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_fold_i64(init: i64, env: *const u8, v: *const GosVec) -> i64 {
    ffi_entry!(-1, {
        if env.is_null() || v.is_null() {
            return init;
        }
        let vec = unsafe { &*v };
        type FoldFn = unsafe extern "C" fn(env: *const u8, acc: i64, x: i64) -> i64;
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return init;
        }
        let f: FoldFn = unsafe { std::mem::transmute(fn_addr_raw) };
        let mut acc = init;
        for i in 0..vec.len {
            let x = unsafe { gos_rt_vec_get_i64(v, i) };
            acc = unsafe { f(env, acc, x) };
        }
        acc
    })
}

/// `iter::sum_by(f, xs)` for `Vec<i64>` -> i64. `f` maps each element
/// to its contribution.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_sum_by_i64(env: *const u8, v: *const GosVec) -> i64 {
    ffi_entry!(-1, {
        if env.is_null() || v.is_null() {
            return 0;
        }
        let vec = unsafe { &*v };
        type MapFn = unsafe extern "C" fn(env: *const u8, x: i64) -> i64;
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return 0;
        }
        let f: MapFn = unsafe { std::mem::transmute(fn_addr_raw) };
        let mut total: i64 = 0;
        for i in 0..vec.len {
            let x = unsafe { gos_rt_vec_get_i64(v, i) };
            total = total.wrapping_add(unsafe { f(env, x) });
        }
        total
    })
}

/// `iter::any(p, xs)` for `Vec<i64>` -> bool (returned as i64 0/1).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_any_i64(env: *const u8, v: *const GosVec) -> i64 {
    ffi_entry!(-1, {
        if env.is_null() || v.is_null() {
            return 0;
        }
        let vec = unsafe { &*v };
        type PredFn = unsafe extern "C" fn(env: *const u8, x: i64) -> bool;
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return 0;
        }
        let p: PredFn = unsafe { std::mem::transmute(fn_addr_raw) };
        for i in 0..vec.len {
            let x = unsafe { gos_rt_vec_get_i64(v, i) };
            if unsafe { p(env, x) } {
                return 1;
            }
        }
        0
    })
}

/// `iter::all(p, xs)` for `Vec<i64>` -> bool (returned as i64 0/1).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_all_i64(env: *const u8, v: *const GosVec) -> i64 {
    ffi_entry!(-1, {
        if env.is_null() || v.is_null() {
            return 1;
        }
        let vec = unsafe { &*v };
        type PredFn = unsafe extern "C" fn(env: *const u8, x: i64) -> bool;
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return 1;
        }
        let p: PredFn = unsafe { std::mem::transmute(fn_addr_raw) };
        for i in 0..vec.len {
            let x = unsafe { gos_rt_vec_get_i64(v, i) };
            if !unsafe { p(env, x) } {
                return 0;
            }
        }
        1
    })
}

/// `iter::find(p, xs)` for `Vec<i64>` -> `(found, value)` packed: returns
/// `(1, x)` for first match and `(0, 0)` for none. Caller pulls the
/// match flag through `gos_rt_iter_find_i64_flag`; this entry returns
/// the value. Two-stage so the same dispatch table can name both.
///
/// In MIR we expose this as `iter::find` producing `Option<i64>` -
/// the lowering builds a `gos_rt_option_new(disc, payload)` from the
/// `(flag, value)` pair so source-level pattern-matching keeps working.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_find_i64(env: *const u8, v: *const GosVec) -> i64 {
    ffi_entry!(-1, {
        if env.is_null() || v.is_null() {
            return 0;
        }
        let vec = unsafe { &*v };
        type PredFn = unsafe extern "C" fn(env: *const u8, x: i64) -> bool;
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return 0;
        }
        let p: PredFn = unsafe { std::mem::transmute(fn_addr_raw) };
        for i in 0..vec.len {
            let x = unsafe { gos_rt_vec_get_i64(v, i) };
            if unsafe { p(env, x) } {
                return x;
            }
        }
        0
    })
}

/// Companion to `gos_rt_iter_find_i64` - returns 1 if some element
/// matched, 0 otherwise. Together they let the lowering synthesize an
/// `Option<i64>` without packing values into wider returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_find_i64_flag(env: *const u8, v: *const GosVec) -> i64 {
    ffi_entry!(-1, {
        if env.is_null() || v.is_null() {
            return 0;
        }
        let vec = unsafe { &*v };
        type PredFn = unsafe extern "C" fn(env: *const u8, x: i64) -> bool;
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return 0;
        }
        let p: PredFn = unsafe { std::mem::transmute(fn_addr_raw) };
        for i in 0..vec.len {
            let x = unsafe { gos_rt_vec_get_i64(v, i) };
            if unsafe { p(env, x) } {
                return 1;
            }
        }
        0
    })
}

// ======================================================================
// std::option - non-closure accessors. The closure-taking option::map /
// and_then / filter / default_with / or_else / iter helpers stay in the
// interp VM only for the moment; they need per-shape thunks across all
// inner types, which is the open piece of the Phase 1b follow-up.

/// `option::is_some(opt)` - opt is the `*mut GosResult`-shaped enum
/// handle produced by the `Option<T>` constructor lowering (disc 0 =
/// Some, 1 = None per `lower_result_ctor`).
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_option_is_some(opt: i128) -> i64 {
    i64::from(super::vec::gos_rt_result_disc(opt) == 0)
}

/// `option::is_none(opt)`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_option_is_none(opt: i128) -> i64 {
    i64::from(super::vec::gos_rt_result_disc(opt) != 0)
}

/// `option::default(v, opt) -> v if opt is None else inner`. Specialised
/// for i64 payloads (the dominant case in arithmetic pipelines).
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_option_default_i64(fallback: i64, opt: i128) -> i64 {
    if super::vec::gos_rt_result_disc(opt) != 0 {
        fallback
    } else {
        super::vec::gos_rt_result_payload(opt)
    }
}

/// `option::default(v, opt)` specialised for f64 payloads: the stored
/// payload word is reinterpreted as its IEEE-754 bit pattern, and the
/// fallback rides the float register directly.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_option_default_f64(fallback: f64, opt: i128) -> f64 {
    if super::vec::gos_rt_result_disc(opt) != 0 {
        fallback
    } else {
        f64::from_bits(super::vec::gos_rt_result_payload(opt) as u64)
    }
}

/// `option::map(f, opt) -> Option<i64>`. Mirrors `iter::map` shape:
/// `env[0]` holds the closure body fn-addr (i64), and the closure
/// is called as `f(env, x) -> i64`. Returns a fresh `*mut GosResult`
/// (disc=0 Some, disc=1 None) so the surrounding pattern match
/// reads the standard discriminant.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_option_map_i64(env: *const u8, opt: i128) -> i128 {
    ffi_entry!(0i128, {
        // None passes through unchanged.
        if env.is_null() {
            return gos_rt_result_new(1, 0);
        }
        if super::vec::gos_rt_result_disc(opt) != 0 {
            return gos_rt_result_new(1, 0);
        }
        let payload = super::vec::gos_rt_result_payload(opt);
        type CallFn = unsafe extern "C" fn(env: *const u8, x: i64) -> i64;
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let f: CallFn = unsafe { std::mem::transmute(fn_addr_raw) };
        let mapped = unsafe { f(env, payload) };
        unsafe { gos_rt_result_new(0, mapped) }
    })
}

/// `result::map(f, res) -> Result<i64, E>`. Mirror of
/// `gos_rt_option_map_i64`: maps Ok payload, passes Err through.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_map_i64(env: *const u8, res: i128) -> i128 {
    ffi_entry!(0i128, {
        if env.is_null() {
            return gos_rt_result_new(1, 0);
        }
        let disc = super::vec::gos_rt_result_disc(res);
        let payload = super::vec::gos_rt_result_payload(res);
        if disc != 0 {
            // Err - pass through.
            return gos_rt_result_new(disc, payload);
        }
        type CallFn = unsafe extern "C" fn(env: *const u8, x: i64) -> i64;
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let f: CallFn = unsafe { std::mem::transmute(fn_addr_raw) };
        let mapped = unsafe { f(env, payload) };
        unsafe { gos_rt_result_new(0, mapped) }
    })
}
