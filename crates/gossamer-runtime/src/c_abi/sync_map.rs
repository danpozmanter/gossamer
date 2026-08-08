#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::wildcard_imports)]

use std::collections::HashMap;
use std::os::raw::c_char;

use super::*;

// ---------------------------------------------------------------
// sync::Map - concurrent String -> String map
// ---------------------------------------------------------------
//
// Wraps `parking_lot::RwLock<HashMap<String, String>>`. Reads
// take the shared lock; writes take the exclusive lock. The
// String value choice mirrors the most common Go `sync.Map`
// caller (caches, session stores, feature-flag maps); callers
// that need richer payload types can JSON-encode through the
// same surface.

pub struct GosSyncMap {
    inner: parking_lot::RwLock<HashMap<String, String>>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_map_new() -> *mut GosSyncMap {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosSyncMap {
            inner: parking_lot::RwLock::new(HashMap::new()),
        }))
    })
}

fn cstr_to_string(p: *const c_char) -> String {
    unsafe { crate::c_abi::gos_str_arg_string(p) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_map_set(
    m: *mut GosSyncMap,
    key: *const c_char,
    value: *const c_char,
) {
    ffi_entry!((), {
        if m.is_null() {
            return;
        }
        let m = unsafe { &*m };
        let k = cstr_to_string(key);
        let v = cstr_to_string(value);
        m.inner.write().insert(k, v);
    });
}

/// Returns `Option<String>` as `*mut GosResult` (disc=0 → Some
/// with c-string payload, disc=1 → None). Mirrors the shape used
/// by `gos_rt_map_get_str` and friends so the MIR dispatcher can
/// pin the destination to `Option<String>` without inventing a
/// fresh result-discriminant convention.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_map_get(m: *mut GosSyncMap, key: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if m.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let k = cstr_to_string(key);
        let guard = unsafe { &*m }.inner.read();
        match guard.get(&k) {
            Some(v) => {
                let cs = alloc_cstring(v.as_bytes()) as i64;
                unsafe { gos_rt_result_new(0, cs) }
            }
            None => unsafe { gos_rt_result_new(1, 0) },
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_map_delete(m: *mut GosSyncMap, key: *const c_char) {
    ffi_entry!((), {
        if m.is_null() {
            return;
        }
        let k = cstr_to_string(key);
        unsafe { &*m }.inner.write().remove(&k);
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_map_len(m: *mut GosSyncMap) -> i64 {
    ffi_entry!(0, {
        if m.is_null() {
            return 0;
        }
        unsafe { &*m }.inner.read().len() as i64
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_map_contains(m: *mut GosSyncMap, key: *const c_char) -> i64 {
    ffi_entry!(0, {
        if m.is_null() {
            return 0;
        }
        let k = cstr_to_string(key);
        i64::from(unsafe { &*m }.inner.read().contains_key(&k))
    })
}

/// Returns the live keys as a `*mut GosVec` of `*c_char`
/// (`Vec<String>` ABI). Order is not guaranteed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_map_keys(
    m: *mut GosSyncMap,
) -> *mut crate::c_abi::vec::GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let v = unsafe {
            crate::c_abi::vec::gos_rt_vec_new_typed(8, crate::c_abi::vec::vec_elem_kind::STRING)
        };
        if m.is_null() {
            return v;
        }
        for key in unsafe { &*m }.inner.read().keys() {
            let cs = alloc_cstring(key.as_bytes());
            let cs_i64 = cs as i64;
            unsafe {
                crate::c_abi::vec::gos_rt_vec_push(v, std::ptr::addr_of!(cs_i64).cast::<u8>());
            }
        }
        v
    })
}
