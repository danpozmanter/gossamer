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

use std::ffi::c_char;

use super::*;

// ---------------------------------------------------------------
// container::heap - min-heap operations over `Vec<i64>` (in-place
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

unsafe fn max_heap_sift_up_i64(buf: *mut i64, start_i: usize) {
    let mut i = start_i;
    while i > 0 {
        let parent = (i - 1) / 2;
        let parent_v = unsafe { *buf.add(parent) };
        let cur_v = unsafe { *buf.add(i) };
        if parent_v < cur_v {
            unsafe { std::ptr::swap(buf.add(parent), buf.add(i)) };
            i = parent;
        } else {
            break;
        }
    }
}

unsafe fn max_heap_sift_down_i64(buf: *mut i64, len: usize, start_i: usize) {
    let mut i = start_i;
    loop {
        let l = 2 * i + 1;
        let r = 2 * i + 2;
        let mut largest = i;
        if l < len && unsafe { *buf.add(l) } > unsafe { *buf.add(largest) } {
            largest = l;
        }
        if r < len && unsafe { *buf.add(r) } > unsafe { *buf.add(largest) } {
            largest = r;
        }
        if largest == i {
            break;
        }
        unsafe { std::ptr::swap(buf.add(largest), buf.add(i)) };
        i = largest;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_max_new_i64() -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), { unsafe { gos_rt_vec_new(8) } })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_max_from_vec_i64(v: *mut GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let heap = if v.is_null() {
            unsafe { gos_rt_vec_new(8) }
        } else {
            unsafe { gos_rt_vec_clone(v) }
        };
        let vec = unsafe { &*heap };
        let len = vec.len as usize;
        if len > 1 {
            let buf = vec.ptr.cast::<i64>();
            for i in (0..=(len / 2)).rev() {
                unsafe { max_heap_sift_down_i64(buf, len, i) };
            }
        }
        heap
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_max_push_i64(v: *mut GosVec, value: i64) {
    ffi_entry!((), {
        if v.is_null() {
            return;
        }
        unsafe { gos_rt_vec_push_i64(v, value) };
        let vec = unsafe { &*v };
        let len = vec.len as usize;
        if len > 1 {
            unsafe { max_heap_sift_up_i64(vec.ptr.cast::<i64>(), len - 1) };
        }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_max_pop_i64(v: *mut GosVec) -> i128 {
    ffi_entry!(super::vec::pack_result(1, 0), {
        if v.is_null() {
            return super::vec::pack_result(1, 0);
        }
        let vec = unsafe { &mut *v };
        if vec.len <= 0 {
            return super::vec::pack_result(1, 0);
        }
        let buf = vec.ptr.cast::<i64>();
        let root = unsafe { *buf };
        let last_idx = (vec.len - 1) as usize;
        if last_idx > 0 {
            unsafe { *buf = *buf.add(last_idx) };
        }
        vec.len -= 1;
        let new_len = vec.len as usize;
        if new_len > 1 {
            unsafe { max_heap_sift_down_i64(buf, new_len, 0) };
        }
        super::vec::pack_result(0, root)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_max_peek_i64(v: *const GosVec) -> i128 {
    ffi_entry!(super::vec::pack_result(1, 0), {
        if v.is_null() {
            return super::vec::pack_result(1, 0);
        }
        let vec = unsafe { &*v };
        if vec.len <= 0 {
            return super::vec::pack_result(1, 0);
        }
        super::vec::pack_result(0, unsafe { *vec.ptr.cast::<i64>() })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_min_new_i64() -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), { unsafe { gos_rt_vec_new(8) } })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_min_from_vec_i64(v: *mut GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let heap = if v.is_null() {
            unsafe { gos_rt_vec_new(8) }
        } else {
            unsafe { gos_rt_vec_clone(v) }
        };
        let vec = unsafe { &*heap };
        let len = vec.len as usize;
        if len > 1 {
            let buf = vec.ptr.cast::<i64>();
            for i in (0..=(len / 2)).rev() {
                unsafe { heap_sift_down_i64(buf, len, i) };
            }
        }
        heap
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_min_push_i64(v: *mut GosVec, value: i64) {
    ffi_entry!((), {
        if v.is_null() {
            return;
        }
        unsafe { gos_rt_vec_push_i64(v, value) };
        let vec = unsafe { &*v };
        let len = vec.len as usize;
        if len > 1 {
            unsafe { heap_sift_up_i64(vec.ptr.cast::<i64>(), len - 1) };
        }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_min_pop_i64(v: *mut GosVec) -> i128 {
    ffi_entry!(super::vec::pack_result(1, 0), {
        if v.is_null() {
            return super::vec::pack_result(1, 0);
        }
        let vec = unsafe { &mut *v };
        if vec.len <= 0 {
            return super::vec::pack_result(1, 0);
        }
        let buf = vec.ptr.cast::<i64>();
        let root = unsafe { *buf };
        let last_idx = (vec.len - 1) as usize;
        if last_idx > 0 {
            unsafe { *buf = *buf.add(last_idx) };
        }
        vec.len -= 1;
        let new_len = vec.len as usize;
        if new_len > 1 {
            unsafe { heap_sift_down_i64(buf, new_len, 0) };
        }
        super::vec::pack_result(0, root)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_min_peek_i64(v: *const GosVec) -> i128 {
    ffi_entry!(super::vec::pack_result(1, 0), {
        if v.is_null() {
            return super::vec::pack_result(1, 0);
        }
        let vec = unsafe { &*v };
        if vec.len <= 0 {
            return super::vec::pack_result(1, 0);
        }
        super::vec::pack_result(0, unsafe { *vec.ptr.cast::<i64>() })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_is_empty(v: *const GosVec) -> i32 {
    ffi_entry!(1, {
        if v.is_null() {
            return 1;
        }
        i32::from(unsafe { (*v).len <= 0 })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_clear(v: *mut GosVec) {
    ffi_entry!((), {
        if v.is_null() {
            return;
        }
        unsafe { (*v).len = 0 };
    });
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
pub unsafe extern "C" fn gos_rt_bheap_peek_i64(v: *const GosVec) -> i128 {
    ffi_entry!(super::vec::pack_result(1, 0), {
        if v.is_null() {
            return super::vec::pack_result(1, 0);
        }
        let vec = unsafe { &*v };
        if vec.len <= 0 {
            return super::vec::pack_result(1, 0);
        }
        super::vec::pack_result(0, unsafe { *vec.ptr.cast::<i64>() })
    })
}

/// Renders `owner [a, b, c]` over the heap array's own order. Both tiers
/// run the same sift routines, so that order is the same sequence of
/// pushes and pops on either.
unsafe fn bheap_format(v: *const GosVec, owner: &str) -> *mut c_char {
    let mut out = String::from(owner);
    out.push_str(" [");
    if !v.is_null() {
        let vec = unsafe { &*v };
        let buf = vec.ptr.cast::<i64>();
        for i in 0..vec.len.max(0) as usize {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&crate::builtins::format_int(unsafe { *buf.add(i) }));
        }
    }
    out.push(']');
    crate::c_abi::string::alloc_cstring(out.as_bytes())
}

/// Format a `MaxHeap` for `{}` / `{:?}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_max_format(v: *const GosVec) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        unsafe { bheap_format(v, "MaxHeap") }
    })
}

/// Format a `MinHeap` for `{}` / `{:?}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_min_format(v: *const GosVec) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        unsafe { bheap_format(v, "MinHeap") }
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

// ---------------------------------------------------------------
// Float elements. A slot holds the `f64`'s bit pattern, so the sift
// compares the value those bits spell rather than the bits' integer
// order (which reverses across the sign bit). Peek reads the root
// without comparing, so the integer entry points serve both.
// ---------------------------------------------------------------

fn slot_as_f64(bits: i64) -> f64 {
    f64::from_bits(bits as u64)
}

unsafe fn heap_sift_up_f64(buf: *mut i64, start_i: usize) {
    let mut i = start_i;
    while i > 0 {
        let parent = (i - 1) / 2;
        let parent_v = slot_as_f64(unsafe { *buf.add(parent) });
        let cur_v = slot_as_f64(unsafe { *buf.add(i) });
        if parent_v > cur_v {
            unsafe { std::ptr::swap(buf.add(parent), buf.add(i)) };
            i = parent;
        } else {
            break;
        }
    }
}

unsafe fn heap_sift_down_f64(buf: *mut i64, len: usize, start_i: usize) {
    let mut i = start_i;
    loop {
        let l = 2 * i + 1;
        let r = 2 * i + 2;
        let mut smallest = i;
        if l < len
            && slot_as_f64(unsafe { *buf.add(l) }) < slot_as_f64(unsafe { *buf.add(smallest) })
        {
            smallest = l;
        }
        if r < len
            && slot_as_f64(unsafe { *buf.add(r) }) < slot_as_f64(unsafe { *buf.add(smallest) })
        {
            smallest = r;
        }
        if smallest == i {
            break;
        }
        unsafe { std::ptr::swap(buf.add(smallest), buf.add(i)) };
        i = smallest;
    }
}

unsafe fn max_heap_sift_up_f64(buf: *mut i64, start_i: usize) {
    let mut i = start_i;
    while i > 0 {
        let parent = (i - 1) / 2;
        let parent_v = slot_as_f64(unsafe { *buf.add(parent) });
        let cur_v = slot_as_f64(unsafe { *buf.add(i) });
        if parent_v < cur_v {
            unsafe { std::ptr::swap(buf.add(parent), buf.add(i)) };
            i = parent;
        } else {
            break;
        }
    }
}

unsafe fn max_heap_sift_down_f64(buf: *mut i64, len: usize, start_i: usize) {
    let mut i = start_i;
    loop {
        let l = 2 * i + 1;
        let r = 2 * i + 2;
        let mut largest = i;
        if l < len
            && slot_as_f64(unsafe { *buf.add(l) }) > slot_as_f64(unsafe { *buf.add(largest) })
        {
            largest = l;
        }
        if r < len
            && slot_as_f64(unsafe { *buf.add(r) }) > slot_as_f64(unsafe { *buf.add(largest) })
        {
            largest = r;
        }
        if largest == i {
            break;
        }
        unsafe { std::ptr::swap(buf.add(largest), buf.add(i)) };
        i = largest;
    }
}

/// Heapifies a `Vec<f64>` snapshot into a max-heap over the float values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_max_from_vec_f64(v: *mut GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let heap = if v.is_null() {
            unsafe { gos_rt_vec_new(8) }
        } else {
            unsafe { gos_rt_vec_clone(v) }
        };
        let vec = unsafe { &*heap };
        let len = vec.len as usize;
        if len > 1 {
            let buf = vec.ptr.cast::<i64>();
            for i in (0..=(len / 2)).rev() {
                unsafe { max_heap_sift_down_f64(buf, len, i) };
            }
        }
        heap
    })
}

/// Pushes a float onto a max-heap, keeping the greatest value at the root.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_max_push_f64(v: *mut GosVec, value: f64) {
    ffi_entry!((), {
        if v.is_null() {
            return;
        }
        unsafe { gos_rt_vec_push_i64(v, value.to_bits() as i64) };
        let vec = unsafe { &*v };
        let len = vec.len as usize;
        if len > 1 {
            unsafe { max_heap_sift_up_f64(vec.ptr.cast::<i64>(), len - 1) };
        }
    });
}

/// Removes and returns the greatest float as `Option<f64>` bits packed into
/// an i128 (disc=0 `Some`, disc=1 `None`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_max_pop_f64(v: *mut GosVec) -> i128 {
    ffi_entry!(super::vec::pack_result(1, 0), {
        if v.is_null() {
            return super::vec::pack_result(1, 0);
        }
        let vec = unsafe { &mut *v };
        if vec.len <= 0 {
            return super::vec::pack_result(1, 0);
        }
        let buf = vec.ptr.cast::<i64>();
        let root = unsafe { *buf };
        let last_idx = (vec.len - 1) as usize;
        if last_idx > 0 {
            unsafe { *buf = *buf.add(last_idx) };
        }
        vec.len -= 1;
        let new_len = vec.len as usize;
        if new_len > 1 {
            unsafe { max_heap_sift_down_f64(buf, new_len, 0) };
        }
        super::vec::pack_result(0, root)
    })
}

/// Heapifies a `Vec<f64>` snapshot into a min-heap over the float values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_min_from_vec_f64(v: *mut GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let heap = if v.is_null() {
            unsafe { gos_rt_vec_new(8) }
        } else {
            unsafe { gos_rt_vec_clone(v) }
        };
        let vec = unsafe { &*heap };
        let len = vec.len as usize;
        if len > 1 {
            let buf = vec.ptr.cast::<i64>();
            for i in (0..=(len / 2)).rev() {
                unsafe { heap_sift_down_f64(buf, len, i) };
            }
        }
        heap
    })
}

/// Pushes a float onto a min-heap, keeping the least value at the root.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_min_push_f64(v: *mut GosVec, value: f64) {
    ffi_entry!((), {
        if v.is_null() {
            return;
        }
        unsafe { gos_rt_vec_push_i64(v, value.to_bits() as i64) };
        let vec = unsafe { &*v };
        let len = vec.len as usize;
        if len > 1 {
            unsafe { heap_sift_up_f64(vec.ptr.cast::<i64>(), len - 1) };
        }
    });
}

/// Removes and returns the least float as `Option<f64>` bits packed into an
/// i128 (disc=0 `Some`, disc=1 `None`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bheap_min_pop_f64(v: *mut GosVec) -> i128 {
    ffi_entry!(super::vec::pack_result(1, 0), {
        if v.is_null() {
            return super::vec::pack_result(1, 0);
        }
        let vec = unsafe { &mut *v };
        if vec.len <= 0 {
            return super::vec::pack_result(1, 0);
        }
        let buf = vec.ptr.cast::<i64>();
        let root = unsafe { *buf };
        let last_idx = (vec.len - 1) as usize;
        if last_idx > 0 {
            unsafe { *buf = *buf.add(last_idx) };
        }
        vec.len -= 1;
        let new_len = vec.len as usize;
        if new_len > 1 {
            unsafe { heap_sift_down_f64(buf, new_len, 0) };
        }
        super::vec::pack_result(0, root)
    })
}
