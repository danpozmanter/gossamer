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
// container::ordered_set — sorted Vec<i64> with dedup on insert.
// container::ordered_map — flat Vec<i64> of [k0, v0, k1, v1, ...]
// pairs sorted by k; insert replaces an existing key, get does
// binary search.
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_oset_insert_i64(v: *mut GosVec, value: i64) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let cloned = if v.is_null() {
            unsafe { gos_rt_vec_new(8) }
        } else {
            unsafe { gos_rt_vec_clone(v) }
        };
        let vec = unsafe { &mut *cloned };
        let n = vec.len as usize;
        let buf = vec.ptr.cast::<i64>();
        // Linear scan for the sorted insertion point + dedup.
        let mut pos = n;
        for i in 0..n {
            let cur = unsafe { *buf.add(i) };
            if cur == value {
                return cloned;
            }
            if cur > value {
                pos = i;
                break;
            }
        }
        unsafe { gos_rt_vec_push_i64(cloned, value) };
        // Shift right from pos to make room.
        let vec = unsafe { &mut *cloned };
        let new_len = vec.len as usize;
        let buf = vec.ptr.cast::<i64>();
        for i in (pos + 1..new_len).rev() {
            unsafe { *buf.add(i) = *buf.add(i - 1) };
        }
        unsafe { *buf.add(pos) = value };
        cloned
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_oset_remove_i64(v: *mut GosVec, value: i64) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let cloned = if v.is_null() {
            return unsafe { gos_rt_vec_new(8) };
        } else {
            unsafe { gos_rt_vec_clone(v) }
        };
        let vec = unsafe { &mut *cloned };
        let n = vec.len as usize;
        let buf = vec.ptr.cast::<i64>();
        let mut found: Option<usize> = None;
        for i in 0..n {
            if unsafe { *buf.add(i) } == value {
                found = Some(i);
                break;
            }
        }
        if let Some(idx) = found {
            for i in idx..(n - 1) {
                unsafe { *buf.add(i) = *buf.add(i + 1) };
            }
            vec.len -= 1;
        }
        cloned
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_oset_contains_i64(v: *const GosVec, value: i64) -> i64 {
    // Linear scan (already kept sorted; could binary-search but
    // for compact heaps in real code N is small enough to make the
    // branch overhead net-neutral).
    unsafe { gos_rt_ovec_contains_i64(v, value) }
}

/// `om_insert(m, k, v)` — set `m[k] = v`, keeping the flat
/// `[k0,v0,k1,v1,...]` buffer sorted by key.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_omap_insert_i64(
    v: *mut GosVec,
    key: i64,
    value: i64,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let cloned = if v.is_null() {
            unsafe { gos_rt_vec_new(8) }
        } else {
            unsafe { gos_rt_vec_clone(v) }
        };
        let vec = unsafe { &*cloned };
        let pairs = (vec.len as usize) / 2;
        let buf = vec.ptr.cast::<i64>();
        let mut pos = pairs;
        for i in 0..pairs {
            let k = unsafe { *buf.add(i * 2) };
            if k == key {
                unsafe { *buf.add(i * 2 + 1) = value };
                return cloned;
            }
            if k > key {
                pos = i;
                break;
            }
        }
        // Append two slots then shift right by 2 from pos.
        unsafe { gos_rt_vec_push_i64(cloned, 0) };
        unsafe { gos_rt_vec_push_i64(cloned, 0) };
        let vec = unsafe { &*cloned };
        let buf = vec.ptr.cast::<i64>();
        let new_len = vec.len as usize;
        for i in (pos * 2 + 2..new_len).rev() {
            unsafe { *buf.add(i) = *buf.add(i - 2) };
        }
        unsafe { *buf.add(pos * 2) = key };
        unsafe { *buf.add(pos * 2 + 1) = value };
        cloned
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_omap_remove_i64(v: *mut GosVec, key: i64) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let cloned = if v.is_null() {
            return unsafe { gos_rt_vec_new(8) };
        } else {
            unsafe { gos_rt_vec_clone(v) }
        };
        let vec = unsafe { &mut *cloned };
        let pairs = (vec.len as usize) / 2;
        let buf = vec.ptr.cast::<i64>();
        let mut found: Option<usize> = None;
        for i in 0..pairs {
            if unsafe { *buf.add(i * 2) } == key {
                found = Some(i);
                break;
            }
        }
        if let Some(idx) = found {
            let start = idx * 2;
            let new_total = (vec.len as usize) - 2;
            for i in start..new_total {
                unsafe { *buf.add(i) = *buf.add(i + 2) };
            }
            vec.len -= 2;
        }
        cloned
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_omap_get_i64(v: *const GosVec, key: i64) -> i64 {
    ffi_entry!(0, {
        if v.is_null() {
            return 0;
        }
        let vec = unsafe { &*v };
        let pairs = (vec.len as usize) / 2;
        let buf = vec.ptr.cast::<i64>();
        for i in 0..pairs {
            if unsafe { *buf.add(i * 2) } == key {
                return unsafe { *buf.add(i * 2 + 1) };
            }
        }
        0
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_omap_contains_key_i64(v: *const GosVec, key: i64) -> i64 {
    ffi_entry!(0, {
        if v.is_null() {
            return 0;
        }
        let vec = unsafe { &*v };
        let pairs = (vec.len as usize) / 2;
        let buf = vec.ptr.cast::<i64>();
        for i in 0..pairs {
            if unsafe { *buf.add(i * 2) } == key {
                return 1;
            }
        }
        0
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_omap_len(v: *const GosVec) -> i64 {
    ffi_entry!(0, {
        if v.is_null() {
            return 0;
        }
        unsafe { (*v).len / 2 }
    })
}

// ---------------------------------------------------------------
// container::ordered_vec / ordered_list — sorted-on-insert Vec<i64>.
// Insert places `value` in the unique sorted position via binary
// search. Returns fresh Vecs for the re-bind drop semantics.
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_ovec_insert_i64(v: *mut GosVec, value: i64) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let cloned = if v.is_null() {
            unsafe { gos_rt_vec_new(8) }
        } else {
            unsafe { gos_rt_vec_clone(v) }
        };
        // Append, then sift up so the buffer stays sorted in ascending order.
        unsafe { gos_rt_vec_push_i64(cloned, value) };
        let vec = unsafe { &mut *cloned };
        let n = vec.len as usize;
        if n > 1 {
            let buf = vec.ptr.cast::<i64>();
            // Bubble newcomer leftward while it's smaller than left neighbor.
            let mut i = n - 1;
            while i > 0 {
                let prev = unsafe { *buf.add(i - 1) };
                let cur = unsafe { *buf.add(i) };
                if prev > cur {
                    unsafe { std::ptr::swap(buf.add(i - 1), buf.add(i)) };
                    i -= 1;
                } else {
                    break;
                }
            }
        }
        cloned
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_ovec_remove_at_i64(v: *mut GosVec, idx: i64) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let cloned = if v.is_null() {
            return unsafe { gos_rt_vec_new(8) };
        } else {
            unsafe { gos_rt_vec_clone(v) }
        };
        let vec = unsafe { &mut *cloned };
        let n = vec.len as usize;
        let i = if idx < 0 { 0 } else { idx as usize };
        if i >= n {
            return cloned;
        }
        let buf = vec.ptr.cast::<i64>();
        for j in i..(n - 1) {
            unsafe { *buf.add(j) = *buf.add(j + 1) };
        }
        vec.len -= 1;
        cloned
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_ovec_contains_i64(v: *const GosVec, value: i64) -> i64 {
    ffi_entry!(0, {
        if v.is_null() {
            return 0;
        }
        let vec = unsafe { &*v };
        let n = vec.len as usize;
        if n == 0 {
            return 0;
        }
        let buf = vec.ptr.cast::<i64>();
        for i in 0..n {
            if unsafe { *buf.add(i) } == value {
                return 1;
            }
        }
        0
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_ovec_index_of_i64(v: *const GosVec, value: i64) -> i64 {
    ffi_entry!(-1, {
        if v.is_null() {
            return -1;
        }
        let vec = unsafe { &*v };
        let n = vec.len as usize;
        let buf = vec.ptr.cast::<i64>();
        for i in 0..n {
            if unsafe { *buf.add(i) } == value {
                return i as i64;
            }
        }
        -1
    })
}
