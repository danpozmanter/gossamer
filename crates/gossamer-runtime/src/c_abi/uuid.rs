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
// uuid — v4 (random) and v7 (timestamp-ordered) UUID generation,
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
// std::iter combinators — AOT runtime helpers.
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

/// Minimum i64 element. Returns `i64::MIN` for empty input (caller
/// should check `iter::count(xs) > 0` first, or use the closure-taking
/// variants when the empty-vs-non-empty distinction matters).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_min_i64(v: *const GosVec) -> i64 {
    ffi_entry!(-1, {
        if v.is_null() {
            return i64::MIN;
        }
        let vec = unsafe { &*v };
        if vec.ptr.is_null() || vec.len <= 0 {
            return i64::MIN;
        }
        let slice = unsafe { std::slice::from_raw_parts(vec.ptr.cast::<i64>(), vec.len as usize) };
        slice.iter().copied().min().unwrap_or(i64::MIN)
    })
}

/// Maximum i64 element. Returns `i64::MIN` for empty input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_max_i64(v: *const GosVec) -> i64 {
    ffi_entry!(-1, {
        if v.is_null() {
            return i64::MIN;
        }
        let vec = unsafe { &*v };
        if vec.ptr.is_null() || vec.len <= 0 {
            return i64::MIN;
        }
        let slice = unsafe { std::slice::from_raw_parts(vec.ptr.cast::<i64>(), vec.len as usize) };
        slice.iter().copied().max().unwrap_or(i64::MIN)
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

// -- Closure-taking iter helpers. Closure ABI: env pointer with
// fn_addr at env[0]. Each helper transmutes env[0] to a specific
// `(env, args...) -> ret` signature determined by the combinator's
// callback contract.

/// `iter::for_each(f, xs)` — call `f(x)` once per element.
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
/// In MIR we expose this as `iter::find` producing `Option<i64>` —
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

/// Companion to `gos_rt_iter_find_i64` — returns 1 if some element
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
// std::option — non-closure accessors. The closure-taking option::map /
// and_then / filter / default_with / or_else / iter helpers stay in the
// interp VM only for the moment; they need per-shape thunks across all
// inner types, which is the open piece of the Phase 1b follow-up.

/// `option::is_some(opt)` — opt is the `*mut GosResult`-shaped enum
/// handle produced by the `Option<T>` constructor lowering (disc 0 =
/// Some, 1 = None per `lower_result_ctor`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_option_is_some(opt: *const u8) -> i64 {
    ffi_entry!(-1, {
        if opt.is_null() {
            return 0;
        }
        // disc lives at byte 0 of the enum handle.
        let disc = unsafe { *opt };
        i64::from(disc == 0)
    })
}

/// `option::is_none(opt)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_option_is_none(opt: *const u8) -> i64 {
    ffi_entry!(-1, {
        if opt.is_null() {
            return 1;
        }
        let disc = unsafe { *opt };
        i64::from(disc != 0)
    })
}

/// `option::default(v, opt) -> v if opt is None else inner`. Specialised
/// for i64 payloads (the dominant case in arithmetic pipelines).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_option_default_i64(fallback: i64, opt: *const u8) -> i64 {
    ffi_entry!(-1, {
        if opt.is_null() {
            return fallback;
        }
        let disc = unsafe { *opt };
        if disc != 0 {
            return fallback;
        }
        // Payload at offset 8 (one word past the disc).
        unsafe { opt.add(8).cast::<i64>().read_unaligned() }
    })
}
