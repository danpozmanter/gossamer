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
// container::heap — min-heap operations over `Vec<i64>` (in-place
// sift up/down). Pair with `Vec::len` to detect empty before
// peek/pop; the sentinel return (0) on empty is documented but the
// caller is expected to check length.
// ---------------------------------------------------------------

unsafe fn heap_sift_up_i64(buf: *mut i64, start_i: usize) {
    let mut i = start_i;
    while i > 0 {
        let parent = (i - 1) / 2;
        let parent_v = unsafe { *buf.add(parent) };
        let cur_v = unsafe { *buf.add(i) };
        if parent_v > cur_v {
            unsafe { std::ptr::swap(buf.add(parent), buf.add(i)) };
            i = parent;
        } else {
            break;
        }
    }
}

unsafe fn heap_sift_down_i64(buf: *mut i64, len: usize, start_i: usize) {
    let mut i = start_i;
    loop {
        let l = 2 * i + 1;
        let r = 2 * i + 2;
        let mut smallest = i;
        if l < len && unsafe { *buf.add(l) } < unsafe { *buf.add(smallest) } {
            smallest = l;
        }
        if r < len && unsafe { *buf.add(r) } < unsafe { *buf.add(smallest) } {
            smallest = r;
        }
        if smallest == i {
            break;
        }
        unsafe { std::ptr::swap(buf.add(smallest), buf.add(i)) };
        i = smallest;
    }
}

/// Push `value` onto a min-heap clone of the input. Returns a
/// fresh Vec so MIR can safely drop the input binding.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_push_i64(v: *mut GosVec, value: i64) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let cloned = if v.is_null() {
            unsafe { gos_rt_vec_new(8) }
        } else {
            unsafe { gos_rt_vec_clone(v) }
        };
        unsafe { gos_rt_vec_push_i64(cloned, value) };
        let vec = unsafe { &*cloned };
        let len = vec.len as usize;
        if len > 1 {
            let buf = vec.ptr.cast::<i64>();
            unsafe { heap_sift_up_i64(buf, len - 1) };
        }
        cloned
    })
}

/// Drop the root of a clone of the input min-heap.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_pop_i64(v: *mut GosVec) -> *mut GosVec {
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
        let last_idx = (vec.len - 1) as usize;
        if last_idx > 0 {
            unsafe { *buf = *buf.add(last_idx) };
        }
        vec.len -= 1;
        let new_len = vec.len as usize;
        if new_len > 1 {
            unsafe { heap_sift_down_i64(buf, new_len, 0) };
        }
        cloned
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_peek_i64(v: *const GosVec) -> i64 {
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
pub unsafe extern "C" fn gos_rt_bheap_len(v: *const GosVec) -> i64 {
    ffi_entry!(0, {
        if v.is_null() {
            return 0;
        }
        unsafe { (*v).len }
    })
}
