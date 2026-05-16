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

// ---------------------------------------------------------------
// Sets — `HashSet<String>` (the most common shape) backed by
// `std::collections::HashSet<String>`. Stored on the heap; the
// pointer is the value seen by user code. Element type is
// erased at the FFI: only String keys are wired today, matching
// the common case in `examples/data_structures.gos`.
// ---------------------------------------------------------------

pub struct GosSet {
    inner: std::collections::HashSet<String>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_new() -> *mut GosSet {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosSet {
            inner: std::collections::HashSet::new(),
        }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_insert(s: *mut GosSet, key: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() || key.is_null() {
            return 0;
        }
        let k = unsafe { CStr::from_ptr(key).to_string_lossy().into_owned() };
        let s = unsafe { &mut *s };
        i64::from(s.inner.insert(k))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_contains(s: *const GosSet, key: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() || key.is_null() {
            return 0;
        }
        let k = unsafe { CStr::from_ptr(key).to_string_lossy().into_owned() };
        let s = unsafe { &*s };
        i64::from(s.inner.contains(&k))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_remove(s: *mut GosSet, key: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() || key.is_null() {
            return 0;
        }
        let k = unsafe { CStr::from_ptr(key).to_string_lossy().into_owned() };
        let s = unsafe { &mut *s };
        i64::from(s.inner.remove(&k))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_len(s: *const GosSet) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() {
            return 0;
        }
        unsafe { (*s).inner.len() as i64 }
    })
}
