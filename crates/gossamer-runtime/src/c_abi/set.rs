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
// Sets - `HashSet<String>` (the most common shape) backed by
// `std::collections::HashSet<String>`. Stored on the heap; the
// pointer is the value seen by user code. Element type is
// erased at the FFI: only String keys are wired today, matching
// the common case in `examples/data_structures.gos`.
// ---------------------------------------------------------------

pub struct GosSet {
    inner: std::collections::HashSet<String>,
    /// Aggregate elements are keyed by their canonical slot bytes and retain
    /// an owned copy of those slots for `iter()` / set algebra.
    struct_inner: rustc_hash::FxHashMap<Box<[u8]>, Box<[u8]>>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_new() -> *mut GosSet {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosSet {
            inner: std::collections::HashSet::new(),
            struct_inner: rustc_hash::FxHashMap::default(),
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
        let bytes = unsafe { CStr::from_ptr(key).to_bytes() };
        // Gossamer strings are always valid UTF-8 at the source level.
        let k: &str = unsafe { std::str::from_utf8_unchecked(bytes) };
        let s = unsafe { &*s };
        i64::from(s.inner.contains(k))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_remove(s: *mut GosSet, key: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() || key.is_null() {
            return 0;
        }
        let bytes = unsafe { CStr::from_ptr(key).to_bytes() };
        // Gossamer strings are always valid UTF-8 at the source level.
        let k: &str = unsafe { std::str::from_utf8_unchecked(bytes) };
        let s = unsafe { &mut *s };
        i64::from(s.inner.remove(k))
    })
}

// `HashSet<i64>` reuses the String-backed set: each i64 key is stored
// as its canonical decimal string. The mapping i64 -> decimal text is
// injective, so set membership semantics are preserved exactly. The
// MIR dispatch routes i64-element sets here and routes `to_vec` to the
// i64 reader that parses the keys back, so iteration order matches the
// VM's numeric sort.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_insert_i64(s: *mut GosSet, key: i64) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() {
            return 0;
        }
        let s = unsafe { &mut *s };
        i64::from(s.inner.insert(key.to_string()))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_contains_i64(s: *const GosSet, key: i64) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() {
            return 0;
        }
        let s = unsafe { &*s };
        i64::from(s.inner.contains(&key.to_string()))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_remove_i64(s: *mut GosSet, key: i64) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() {
            return 0;
        }
        let s = unsafe { &mut *s };
        i64::from(s.inner.remove(&key.to_string()))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_len(s: *const GosSet) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() {
            return 0;
        }
        let s = unsafe { &*s };
        (s.inner.len() + s.struct_inner.len()) as i64
    })
}

/// Snapshots a string set's keys into a fresh `Vec<String>`, sorted
/// lexicographically so iteration order is deterministic and matches
/// the VM's sorted `to_vec`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_to_vec(s: *const GosSet) -> *mut crate::c_abi::vec::GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe {
            crate::c_abi::vec::gos_rt_vec_new_typed(8, crate::c_abi::vec::vec_elem_kind::STRING)
        };
        if s.is_null() {
            return out;
        }
        let s = unsafe { &*s };
        let mut keys: Vec<&str> = s.inner.iter().map(String::as_str).collect();
        keys.sort_unstable();
        for k in keys {
            let cstr = crate::c_abi::string::alloc_cstring(k.as_bytes());
            let slot = (cstr as usize as i64).to_ne_bytes();
            unsafe { crate::c_abi::vec::gos_rt_vec_push(out, slot.as_ptr()) };
        }
        out
    })
}

/// Snapshots an i64 set's keys into a fresh `Vec<i64>`, sorted
/// numerically to match the VM's `MapKey::Int` ordering. The keys are
/// stored as decimal text (see `gos_rt_set_insert_i64`) and parsed back.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_to_vec_i64(s: *const GosSet) -> *mut crate::c_abi::vec::GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe {
            crate::c_abi::vec::gos_rt_vec_new_typed(8, crate::c_abi::vec::vec_elem_kind::PRIMITIVE)
        };
        if s.is_null() {
            return out;
        }
        let s = unsafe { &*s };
        let mut keys: Vec<i64> = s
            .inner
            .iter()
            .filter_map(|k| k.parse::<i64>().ok())
            .collect();
        keys.sort_unstable();
        for k in keys {
            unsafe { crate::c_abi::vec::gos_rt_vec_push_i64(out, k) };
        }
        out
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_clear(s: *mut GosSet) -> *mut GosSet {
    ffi_entry!(s, {
        if !s.is_null() {
            unsafe { (*s).inner.clear() };
        }
        s
    })
}

/// Borrows the two operand sets, or returns empty borrows for null
/// pointers so the algebra shims never deref a null handle.
unsafe fn set_pair<'a>(
    a: *const GosSet,
    b: *const GosSet,
) -> (
    &'a std::collections::HashSet<String>,
    &'a std::collections::HashSet<String>,
) {
    static EMPTY: std::sync::OnceLock<std::collections::HashSet<String>> =
        std::sync::OnceLock::new();
    let empty = EMPTY.get_or_init(std::collections::HashSet::new);
    let a = if a.is_null() {
        empty
    } else {
        unsafe { &(*a).inner }
    };
    let b = if b.is_null() {
        empty
    } else {
        unsafe { &(*b).inner }
    };
    (a, b)
}

unsafe fn set_from(inner: std::collections::HashSet<String>) -> *mut GosSet {
    Box::into_raw(Box::new(GosSet {
        inner,
        struct_inner: rustc_hash::FxHashMap::default(),
    }))
}

/// Inserts a struct or tuple by value. `desc` is the same slot descriptor as
/// the aggregate-keyed HashMap ABI, so equal values at different addresses
/// remain equal in the native runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_insert_skey(
    s: *mut GosSet,
    key: *const u8,
    desc: *const c_char,
) -> i64 {
    ffi_entry!(-1, {
        let Some(canonical) = (unsafe { crate::c_abi::map::build_skey_for_set(key, desc) }) else {
            return 0;
        };
        if s.is_null() {
            return 0;
        }
        let width = unsafe { CStr::from_ptr(desc) }.to_bytes().len() * 8;
        let slots = unsafe { std::slice::from_raw_parts(key, width) }
            .to_vec()
            .into_boxed_slice();
        let s = unsafe { &mut *s };
        i64::from(
            s.struct_inner
                .insert(canonical.into_boxed_slice(), slots)
                .is_none(),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_contains_skey(
    s: *const GosSet,
    key: *const u8,
    desc: *const c_char,
) -> i64 {
    ffi_entry!(-1, {
        let Some(canonical) = (unsafe { crate::c_abi::map::build_skey_for_set(key, desc) }) else {
            return 0;
        };
        if s.is_null() {
            return 0;
        }
        let s = unsafe { &*s };
        i64::from(s.struct_inner.contains_key(canonical.as_slice()))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_remove_skey(
    s: *mut GosSet,
    key: *const u8,
    desc: *const c_char,
) -> i64 {
    ffi_entry!(-1, {
        let Some(canonical) = (unsafe { crate::c_abi::map::build_skey_for_set(key, desc) }) else {
            return 0;
        };
        if s.is_null() {
            return 0;
        }
        let s = unsafe { &mut *s };
        i64::from(s.struct_inner.remove(canonical.as_slice()).is_some())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_to_vec_skey(
    s: *const GosSet,
    desc: *const c_char,
) -> *mut crate::c_abi::vec::GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        if desc.is_null() {
            return std::ptr::null_mut();
        }
        let width = unsafe { CStr::from_ptr(desc) }.to_bytes().len() * 8;
        let out = unsafe {
            crate::c_abi::vec::gos_rt_vec_new_typed(
                width as u32,
                crate::c_abi::vec::vec_elem_kind::PRIMITIVE,
            )
        };
        if s.is_null() {
            return out;
        }
        let s = unsafe { &*s };
        let mut entries: Vec<_> = s.struct_inner.iter().collect();
        entries.sort_unstable_by_key(|(key, _)| *key);
        for (_, slots) in entries {
            unsafe { crate::c_abi::vec::gos_rt_vec_push(out, slots.as_ptr()) };
        }
        out
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_union(a: *const GosSet, b: *const GosSet) -> *mut GosSet {
    ffi_entry!(std::ptr::null_mut(), {
        let (a, b) = unsafe { set_pair(a, b) };
        unsafe { set_from(a.union(b).cloned().collect()) }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_intersection(
    a: *const GosSet,
    b: *const GosSet,
) -> *mut GosSet {
    ffi_entry!(std::ptr::null_mut(), {
        let (a, b) = unsafe { set_pair(a, b) };
        unsafe { set_from(a.intersection(b).cloned().collect()) }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_intersection_skey(
    a: *const GosSet,
    b: *const GosSet,
) -> *mut GosSet {
    ffi_entry!(std::ptr::null_mut(), {
        let mut out = GosSet {
            inner: std::collections::HashSet::new(),
            struct_inner: rustc_hash::FxHashMap::default(),
        };
        if a.is_null() || b.is_null() {
            return Box::into_raw(Box::new(out));
        }
        let (a, b) = unsafe { (&*a, &*b) };
        for (key, slots) in &a.struct_inner {
            if b.struct_inner.contains_key(key) {
                out.struct_inner.insert(key.clone(), slots.clone());
            }
        }
        Box::into_raw(Box::new(out))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_difference(a: *const GosSet, b: *const GosSet) -> *mut GosSet {
    ffi_entry!(std::ptr::null_mut(), {
        let (a, b) = unsafe { set_pair(a, b) };
        unsafe { set_from(a.difference(b).cloned().collect()) }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_symmetric_difference(
    a: *const GosSet,
    b: *const GosSet,
) -> *mut GosSet {
    ffi_entry!(std::ptr::null_mut(), {
        let (a, b) = unsafe { set_pair(a, b) };
        unsafe { set_from(a.symmetric_difference(b).cloned().collect()) }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_is_subset(a: *const GosSet, b: *const GosSet) -> i64 {
    ffi_entry!(-1, {
        let (a, b) = unsafe { set_pair(a, b) };
        i64::from(a.is_subset(b))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_is_superset(a: *const GosSet, b: *const GosSet) -> i64 {
    ffi_entry!(-1, {
        let (a, b) = unsafe { set_pair(a, b) };
        i64::from(a.is_superset(b))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_is_disjoint(a: *const GosSet, b: *const GosSet) -> i64 {
    ffi_entry!(-1, {
        let (a, b) = unsafe { set_pair(a, b) };
        i64::from(a.is_disjoint(b))
    })
}
