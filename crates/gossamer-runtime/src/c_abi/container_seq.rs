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

use super::*;

// ---------------------------------------------------------------
// container::queue / stack / deque — sequence container ops over
// `Vec<i64>`. All operations return the same heap pointer for the
// `let q = queue::push(q, v)` re-bind idiom.
// ---------------------------------------------------------------

/// Append `value` to a clone of the input Vec<i64>. Returns a
/// fresh Vec so the caller's old binding can be dropped cleanly
/// (MIR's let-shadowing pattern is sensitive to same-pointer
/// returns — see `containers_seq_demo.gos` for the canonical
/// re-bind shape).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_push_back_i64(v: *mut GosVec, value: i64) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let cloned = if v.is_null() {
            unsafe { gos_rt_vec_new(8) }
        } else {
            unsafe { gos_rt_vec_clone(v) }
        };
        unsafe { gos_rt_vec_push_i64(cloned, value) };
        cloned
    })
}

/// Returns the first i64 in the vec, or 0 if empty (caller checks
/// length first). The Option-returning variant `gos_rt_vec_first`
/// boxes through `*mut GosOption`; this scalar variant is used by
/// the queue/deque `peek_front` paths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_first_i64(v: *const GosVec) -> i64 {
    ffi_entry!(0, {
        if v.is_null() {
            return 0;
        }
        let vec = unsafe { &*v };
        if vec.len <= 0 {
            return 0;
        }
        unsafe { *vec.ptr.cast::<i64>() }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_last_i64(v: *const GosVec) -> i64 {
    ffi_entry!(0, {
        if v.is_null() {
            return 0;
        }
        let vec = unsafe { &*v };
        if vec.len <= 0 {
            return 0;
        }
        let idx = (vec.len - 1) as usize;
        unsafe { *vec.ptr.cast::<i64>().add(idx) }
    })
}

/// Drop the front i64 from a clone of the input. Returns a fresh
/// Vec (see `gos_rt_vec_push_back_i64` for the rebind rationale).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_pop_front_i64(v: *mut GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let cloned = if v.is_null() {
            return unsafe { gos_rt_vec_new(8) };
        } else {
            unsafe { gos_rt_vec_clone(v) }
        };
        let vec = unsafe { &mut *cloned };
        if vec.len <= 0 {
            return cloned;
        }
        let buf = vec.ptr.cast::<i64>();
        let n = vec.len as usize;
        for i in 0..(n - 1) {
            unsafe { *buf.add(i) = *buf.add(i + 1) };
        }
        vec.len -= 1;
        cloned
    })
}

/// Drop the back i64 from a clone of the input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_pop_back_i64(v: *mut GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let cloned = if v.is_null() {
            return unsafe { gos_rt_vec_new(8) };
        } else {
            unsafe { gos_rt_vec_clone(v) }
        };
        let vec = unsafe { &mut *cloned };
        if vec.len > 0 {
            vec.len -= 1;
        }
        cloned
    })
}

/// Prepend `value` to a clone of the input Vec.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_push_front_i64(v: *mut GosVec, value: i64) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let cloned = if v.is_null() {
            unsafe { gos_rt_vec_new(8) }
        } else {
            unsafe { gos_rt_vec_clone(v) }
        };
        // Append sentinel to grow the buffer by one slot.
        unsafe { gos_rt_vec_push_i64(cloned, 0) };
        let vec = unsafe { &mut *cloned };
        let buf = vec.ptr.cast::<i64>();
        let n = vec.len as usize;
        for i in (1..n).rev() {
            unsafe { *buf.add(i) = *buf.add(i - 1) };
        }
        unsafe { *buf = value };
        cloned
    })
}
