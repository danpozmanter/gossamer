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
// HashMap runtime — typed-storage variants over rustc-hash's
// FxHashMap. Auto-promotes Empty → I64I64 / StrI64 / StrStr /
// Bytes on first typed call. The i64-keyed/i64-valued shape
// (counter / scoreboard hot paths) avoids per-op `Vec<u8>`
// allocation and uses FxHash directly on the
// 8-byte key.
// ---------------------------------------------------------------

use rustc_hash::FxHashMap;

/// Layout-sensitive: the first 8 bytes hold the current element
/// count so the generic `gos_rt_arr_len` returns the right value
/// without needing a HashMap-specific dispatch.
#[repr(C)]
pub struct GosMap {
    len_cache: i64,
    storage: parking_lot::Mutex<MapStorage>,
}

enum MapStorage {
    Empty,
    I64I64(FxHashMap<i64, i64>),
    /// String-keyed maps store keys as `Box<[u8]>` (16 B header)
    /// rather than `Vec<u8>` (24 B header) — for the k-mer-counter
    /// hot shape (HashMap<String, i64> with millions of short
    /// keys), the saved 8 B per entry compounds visibly: ~8 MB
    /// off a 1 M-entry table. Same applies to `StrStr` keys and
    /// the `Bytes` byte-erased fallback.
    StrI64(FxHashMap<Box<[u8]>, i64>),
    StrStr(FxHashMap<Box<[u8]>, Box<[u8]>>),
    I64Str(FxHashMap<i64, Box<[u8]>>),
    Bytes(FxHashMap<Box<[u8]>, Box<[u8]>>),
    /// Struct / aggregate keys: the key is the flat content bytes of the
    /// aggregate (so two distinct allocations of an equal value hash and
    /// compare equal, matching the VM), the value is an 8-byte word — an
    /// `i64`, or a heap pointer for `String` / struct values.
    SkeyVal(FxHashMap<Box<[u8]>, i64>),
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_new(_key_bytes: u32, _val_bytes: u32) -> *mut GosMap {
    ffi_entry!(std::ptr::null_mut(), {
        crate::c_abi::ledger::map_inc();
        Box::into_raw(Box::new(GosMap {
            len_cache: 0,
            storage: parking_lot::Mutex::new(MapStorage::Empty),
        }))
    })
}

/// Pre-sized constructor: avoids the doubling chain (~22 reallocs
/// for ~5M inserts) when the caller has an upper bound. Picks the
/// initial typed shape from the byte sizes — both 8 → I64I64,
/// otherwise the byte-erased generic shape that promotes lazily.
/// Pre-sizing avoids the doubling chain on counter-style hot
/// loops where the caller knows the total entry count.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_new_with_capacity(
    key_bytes: u32,
    val_bytes: u32,
    cap: i64,
) -> *mut GosMap {
    ffi_entry!(std::ptr::null_mut(), {
        let cap = if cap < 0 { 0 } else { cap as usize };
        let storage = if key_bytes == 8 && val_bytes == 8 {
            MapStorage::I64I64(FxHashMap::with_capacity_and_hasher(
                cap,
                rustc_hash::FxBuildHasher,
            ))
        } else {
            MapStorage::Empty
        };
        Box::into_raw(Box::new(GosMap {
            len_cache: 0,
            storage: parking_lot::Mutex::new(storage),
        }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_len(m: *const GosMap) -> i64 {
    ffi_entry!(-1, {
        if m.is_null() {
            return 0;
        }
        unsafe { (*m).len_cache }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_insert(m: *mut GosMap, key: *const u8, val: *const u8) {
    ffi_entry!((), {
        if m.is_null() || key.is_null() || val.is_null() {
            return;
        }
        let map = unsafe { &mut *m };
        let k = unsafe { std::slice::from_raw_parts(key, 8) }.to_vec();
        let v = unsafe { std::slice::from_raw_parts(val, 8) }.to_vec();
        let mut storage = map.storage.lock();
        if matches!(*storage, MapStorage::Empty) {
            *storage = MapStorage::Bytes(FxHashMap::default());
        }
        let MapStorage::Bytes(inner) = &mut *storage else {
            return;
        };
        if inner
            .insert(k.into_boxed_slice(), v.into_boxed_slice())
            .is_none()
        {
            map.len_cache += 1;
        }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get(m: *const GosMap, key: *const u8, val_out: *mut u8) -> i32 {
    ffi_entry!(-1, {
        if m.is_null() || key.is_null() || val_out.is_null() {
            return 0;
        }
        let map = unsafe { &*m };
        let k = unsafe { std::slice::from_raw_parts(key, 8) };
        let storage = map.storage.lock();
        let MapStorage::Bytes(inner) = &*storage else {
            return 0;
        };
        if let Some(v) = inner.get(k) {
            unsafe {
                std::ptr::copy_nonoverlapping(v.as_ptr(), val_out, v.len());
            }
            1
        } else {
            0
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_or_i64(m: *const GosMap, key: i64, default: i64) -> i64 {
    ffi_entry!(-1, {
        if m.is_null() {
            return default;
        }
        let map = unsafe { &*m };
        let storage = map.storage.lock();
        match &*storage {
            MapStorage::I64I64(inner) => inner.get(&key).copied().unwrap_or(default),
            _ => default,
        }
    })
}

/// `get_or` for string-keyed, i64-valued maps. Mirrors
/// `gos_rt_map_get_or_i64` but hashes the key via the same UTF-8
/// byte slice the `_str_i64` insert path uses, so an `insert(k, v)`
/// followed by `get_or(k, d)` round-trips.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_or_str_i64(
    m: *const GosMap,
    key: *const c_char,
    default: i64,
) -> i64 {
    ffi_entry!(-1, {
        if m.is_null() || key.is_null() {
            return default;
        }
        let map = unsafe { &*m };
        let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes();
        let storage = map.storage.lock();
        match &*storage {
            MapStorage::StrI64(inner) => inner.get(key_bytes).copied().unwrap_or(default),
            _ => default,
        }
    })
}

/// `get_or` for string-keyed, string-valued maps. Returns a fresh
/// GC-allocated `*mut c_char` for the stored value, or a copy of
/// `default`'s bytes when the key is absent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_or_str_str(
    m: *const GosMap,
    key: *const c_char,
    default: *const c_char,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let default_bytes: &[u8] = if default.is_null() {
            b""
        } else {
            unsafe { CStr::from_ptr(default) }.to_bytes()
        };
        if m.is_null() || key.is_null() {
            return alloc_cstring(default_bytes);
        }
        let map = unsafe { &*m };
        let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes();
        let storage = map.storage.lock();
        let MapStorage::StrStr(inner) = &*storage else {
            return alloc_cstring(default_bytes);
        };
        match inner.get(key_bytes) {
            Some(v) => alloc_cstring(v),
            None => alloc_cstring(default_bytes),
        }
    })
}

/// `get_or` for i64-keyed, string-valued maps.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_or_i64_str(
    m: *const GosMap,
    key: i64,
    default: *const c_char,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let default_bytes: &[u8] = if default.is_null() {
            b""
        } else {
            unsafe { CStr::from_ptr(default) }.to_bytes()
        };
        if m.is_null() {
            return alloc_cstring(default_bytes);
        }
        let map = unsafe { &*m };
        let storage = map.storage.lock();
        let MapStorage::I64Str(inner) = &*storage else {
            return alloc_cstring(default_bytes);
        };
        match inner.get(&key) {
            Some(v) => alloc_cstring(v),
            None => alloc_cstring(default_bytes),
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_insert_i64_i64(m: *mut GosMap, key: i64, val: i64) {
    ffi_entry!((), {
        if m.is_null() {
            return;
        }
        let map = unsafe { &mut *m };
        let mut storage = map.storage.lock();
        if matches!(*storage, MapStorage::Empty) {
            *storage = MapStorage::I64I64(FxHashMap::default());
        }
        let MapStorage::I64I64(inner) = &mut *storage else {
            return;
        };
        if inner.insert(key, val).is_none() {
            map.len_cache += 1;
        }
    });
}

/// Builds a canonical by-value key for an aggregate from its flat slot buffer,
/// driven by a per-slot layout descriptor: `'s'` = an 8-byte scalar (read
/// inline), `'S'` = a `String` pointer (dereferenced; its length-prefixed
/// content is folded in). Nested all-scalar structs inline their slots, so
/// they appear as runs of `'s'`. The result is identical for two equal values
/// at distinct allocations, matching the VM's value-keying.
unsafe fn build_skey(key: *const u8, desc: *const c_char) -> Option<Vec<u8>> {
    if key.is_null() || desc.is_null() {
        return None;
    }
    let desc = unsafe { CStr::from_ptr(desc) }.to_bytes();
    let mut out = Vec::with_capacity(desc.len() * 8);
    let mut off = 0usize;
    for &c in desc {
        let slot = unsafe { key.add(off) };
        match c {
            b's' => out.extend_from_slice(unsafe { std::slice::from_raw_parts(slot, 8) }),
            b'S' => {
                let sptr = unsafe { *(slot as *const *const c_char) };
                if sptr.is_null() {
                    out.extend_from_slice(&0u64.to_le_bytes());
                } else {
                    let bytes = unsafe { CStr::from_ptr(sptr) }.to_bytes();
                    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
                    out.extend_from_slice(bytes);
                }
            }
            _ => return None,
        }
        off += 8;
    }
    Some(out)
}

/// Struct-keyed insert: keys by the aggregate's content (per `desc`) rather
/// than its pointer, so two equal values share a slot, matching the VM. `val`
/// is the 8-byte value word (an `i64`, or a pointer for `String` / struct
/// values).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_insert_skey(
    m: *mut GosMap,
    key: *const u8,
    desc: *const c_char,
    val: i64,
) {
    ffi_entry!((), {
        let Some(k) = (unsafe { build_skey(key, desc) }) else {
            return;
        };
        if m.is_null() {
            return;
        }
        let map = unsafe { &mut *m };
        let mut storage = map.storage.lock();
        if matches!(*storage, MapStorage::Empty) {
            *storage = MapStorage::SkeyVal(FxHashMap::default());
        }
        let MapStorage::SkeyVal(inner) = &mut *storage else {
            return;
        };
        if inner.insert(k.into_boxed_slice(), val).is_none() {
            map.len_cache += 1;
        }
    });
}

/// Struct-keyed lookup. Returns `Option<i64>` in the `gos_rt_result_new`
/// i128 layout (0 = Some, 1 = None), matching [`gos_rt_map_get_i64_opt`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_skey_opt(
    m: *const GosMap,
    key: *const u8,
    desc: *const c_char,
) -> i128 {
    ffi_entry!(unsafe { gos_rt_result_new(1, 0) }, {
        let none = unsafe { gos_rt_result_new(1, 0) };
        let Some(k) = (unsafe { build_skey(key, desc) }) else {
            return none;
        };
        if m.is_null() {
            return none;
        }
        let map = unsafe { &*m };
        let storage = map.storage.lock();
        let payload: Option<i64> = match &*storage {
            MapStorage::SkeyVal(inner) => inner.get(k.as_slice()).copied(),
            _ => None,
        };
        match payload {
            Some(v) => unsafe { gos_rt_result_new(0, v) },
            None => none,
        }
    })
}

/// Struct-keyed membership test (for `HashSet` of structs).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_contains_skey(
    m: *const GosMap,
    key: *const u8,
    desc: *const c_char,
) -> bool {
    ffi_entry!(false, {
        let Some(k) = (unsafe { build_skey(key, desc) }) else {
            return false;
        };
        if m.is_null() {
            return false;
        }
        let map = unsafe { &*m };
        let storage = map.storage.lock();
        match &*storage {
            MapStorage::SkeyVal(inner) => inner.contains_key(k.as_slice()),
            _ => false,
        }
    })
}

/// Fused increment: `m[k] = m.get_or(k, 0) + by`. Single lock,
/// single hash, single bucket walk. Replaces the
/// `m.insert(k, m.get_or(k, 0) + 1)` pattern that costs 2× the
/// hash work on hot counter loops.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_inc_i64(m: *mut GosMap, key: i64, by: i64) -> i64 {
    ffi_entry!(-1, {
        if m.is_null() {
            return 0;
        }
        let map = unsafe { &mut *m };
        let mut storage = map.storage.lock();
        if matches!(*storage, MapStorage::Empty) {
            *storage = MapStorage::I64I64(FxHashMap::default());
        }
        let MapStorage::I64I64(inner) = &mut *storage else {
            return 0;
        };
        let entry = inner.entry(key).or_insert_with(|| {
            map.len_cache += 1;
            0
        });
        *entry += by;
        *entry
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_i64(m: *const GosMap, key: i64) -> i64 {
    ffi_entry!(-1, {
        if m.is_null() {
            return 0;
        }
        let map = unsafe { &*m };
        let storage = map.storage.lock();
        match &*storage {
            MapStorage::I64I64(inner) => inner.get(&key).copied().unwrap_or(0),
            _ => 0,
        }
    })
}

/// `m.get(k) -> Option<V>` for an i64-keyed map. Returns `*mut GosResult`
/// with `disc=0, payload=value-as-i64` when present, `disc=1, payload=0`
/// when absent. Treats every 8-byte value payload (i64, c-string ptr,
/// struct heap pointer) uniformly: the MIR pin recovers the proper V
/// from the call expression's `Option<V>` Adt substs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_i64_opt(m: *const GosMap, key: i64) -> i128 {
    ffi_entry!(0i128, {
        if m.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let map = unsafe { &*m };
        let storage = map.storage.lock();
        let payload: Option<i64> = match &*storage {
            MapStorage::I64I64(inner) => inner.get(&key).copied(),
            MapStorage::I64Str(inner) => inner.get(&key).map(|bs| alloc_cstring(bs) as i64),
            _ => None,
        };
        match payload {
            Some(v) => unsafe { gos_rt_result_new(0, v) },
            None => unsafe { gos_rt_result_new(1, 0) },
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_contains_key_i64(m: *const GosMap, key: i64) -> bool {
    ffi_entry!(false, {
        if m.is_null() {
            return false;
        }
        let map = unsafe { &*m };
        let storage = map.storage.lock();
        match &*storage {
            MapStorage::I64I64(inner) => inner.contains_key(&key),
            MapStorage::I64Str(inner) => inner.contains_key(&key),
            _ => false,
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_remove_i64(m: *mut GosMap, key: i64) -> bool {
    ffi_entry!(false, {
        if m.is_null() {
            return false;
        }
        let map = unsafe { &mut *m };
        let mut storage = map.storage.lock();
        let removed = match &mut *storage {
            MapStorage::I64I64(inner) => inner.remove(&key).is_some(),
            MapStorage::I64Str(inner) => inner.remove(&key).is_some(),
            _ => false,
        };
        if removed {
            map.len_cache -= 1;
        }
        removed
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_insert_str_i64(m: *mut GosMap, key: *const c_char, val: i64) {
    ffi_entry!((), {
        if m.is_null() || key.is_null() {
            return;
        }
        let map = unsafe { &mut *m };
        let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes().to_vec();
        let mut storage = map.storage.lock();
        if matches!(*storage, MapStorage::Empty) {
            *storage = MapStorage::StrI64(FxHashMap::default());
        }
        let MapStorage::StrI64(inner) = &mut *storage else {
            return;
        };
        if inner.insert(key_bytes.into_boxed_slice(), val).is_none() {
            map.len_cache += 1;
        }
        drop(storage);
        // Consuming insert copied the key bytes; release the moved-in gos-string
        // (rc-aware + tag-checked — safe for temps, shared, and literals).
        unsafe { gos_rt_str_free(key.cast_mut()) };
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_str_i64(m: *const GosMap, key: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if m.is_null() || key.is_null() {
            return 0;
        }
        let map = unsafe { &*m };
        let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes();
        let storage = map.storage.lock();
        match &*storage {
            MapStorage::StrI64(inner) => inner.get(key_bytes).copied().unwrap_or(0),
            _ => 0,
        }
    })
}

/// `m.get(k) -> Option<V>` for a string-keyed map. Same `*mut GosResult`
/// layout as [`gos_rt_map_get_i64_opt`]: 8-byte payload, MIR pin
/// recovers V from the call's `Option<V>` substs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_str_opt(m: *const GosMap, key: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if m.is_null() || key.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let map = unsafe { &*m };
        let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes();
        let storage = map.storage.lock();
        let payload: Option<i64> = match &*storage {
            MapStorage::StrI64(inner) => inner.get(key_bytes).copied(),
            MapStorage::StrStr(inner) | MapStorage::Bytes(inner) => {
                inner.get(key_bytes).map(|bs| alloc_cstring(bs) as i64)
            }
            _ => None,
        };
        match payload {
            Some(v) => unsafe { gos_rt_result_new(0, v) },
            None => unsafe { gos_rt_result_new(1, 0) },
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_insert_str_str(
    m: *mut GosMap,
    key: *const c_char,
    val: *const c_char,
) {
    ffi_entry!((), {
        if m.is_null() || key.is_null() || val.is_null() {
            return;
        }
        let map = unsafe { &mut *m };
        let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes().to_vec();
        let val_bytes = unsafe { CStr::from_ptr(val) }.to_bytes().to_vec();
        let mut storage = map.storage.lock();
        if matches!(*storage, MapStorage::Empty) {
            *storage = MapStorage::StrStr(FxHashMap::default());
        }
        let MapStorage::StrStr(inner) = &mut *storage else {
            return;
        };
        if inner
            .insert(key_bytes.into_boxed_slice(), val_bytes.into_boxed_slice())
            .is_none()
        {
            map.len_cache += 1;
        }
        drop(storage);
        // The map copied the key/val bytes into its own storage, so it does not
        // retain the inbound gos-strings. `map_insert` is a consuming call (the
        // drop pass moves its arguments in), so release the originals here —
        // `gos_rt_str_free` is rc-aware and tag-checked: a moved temp is freed,
        // a still-shared string only has its count decremented, and a `.rodata`
        // literal / region string is skipped. Without this the inbound
        // `format!(...)` temporaries leaked once per insert.
        unsafe {
            gos_rt_str_free(key.cast_mut());
            gos_rt_str_free(val.cast_mut());
        }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_str_str(
    m: *const GosMap,
    key: *const c_char,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if m.is_null() || key.is_null() {
            return empty_cstring();
        }
        let map = unsafe { &*m };
        let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes();
        let storage = map.storage.lock();
        let MapStorage::StrStr(inner) = &*storage else {
            return empty_cstring();
        };
        match inner.get(key_bytes) {
            Some(v) => alloc_cstring(v),
            None => empty_cstring(),
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_contains_key_str(m: *const GosMap, key: *const c_char) -> bool {
    ffi_entry!(false, {
        if m.is_null() || key.is_null() {
            return false;
        }
        let map = unsafe { &*m };
        let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes();
        let storage = map.storage.lock();
        match &*storage {
            MapStorage::StrI64(inner) => inner.contains_key(key_bytes),
            MapStorage::StrStr(inner) => inner.contains_key(key_bytes),
            _ => false,
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_remove_str(m: *mut GosMap, key: *const c_char) -> bool {
    ffi_entry!(false, {
        if m.is_null() || key.is_null() {
            return false;
        }
        let map = unsafe { &mut *m };
        let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes();
        let mut storage = map.storage.lock();
        let removed = match &mut *storage {
            MapStorage::StrI64(inner) => inner.remove(key_bytes).is_some(),
            MapStorage::StrStr(inner) => inner.remove(key_bytes).is_some(),
            _ => false,
        };
        if removed {
            map.len_cache -= 1;
        }
        removed
    })
}

/// `m.inc_at(seq, start, len, by)` for `HashMap<String, i64>` —
/// the zero-allocation analogue of
/// `m.insert(k, m.get_or(k, 0) + by)` where `k = seq[start..start+len]`.
///
/// Mirrors `*m.entry(&seq[i..i+k]).or_insert(0) += by`: the
/// slice is borrowed (zero-copy), the hash table is consulted
/// exactly once, and a `Vec<u8>` is allocated only on the first
/// occurrence of each unique key. Halves the hash work per
/// iteration vs the get_or + insert pair, and avoids any
/// per-iteration scratch allocation for the key.
///
/// Returns the new value at `seq[start..start+len]` (or `by` if
/// the entry is fresh).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_inc_at_str_i64(
    m: *mut GosMap,
    seq: *const c_char,
    start: i64,
    len: i64,
    by: i64,
) -> i64 {
    ffi_entry!(-1, {
        if m.is_null() || seq.is_null() || len <= 0 || start < 0 {
            return 0;
        }
        let map = unsafe { &mut *m };
        let key_slice: &[u8] = unsafe {
            std::slice::from_raw_parts(seq.cast::<u8>().add(start as usize), len as usize)
        };
        let mut storage = map.storage.lock();
        if matches!(*storage, MapStorage::Empty) {
            *storage = MapStorage::StrI64(FxHashMap::default());
        }
        let MapStorage::StrI64(inner) = &mut *storage else {
            return 0;
        };
        // Lookup is by `&[u8]` — `Vec<u8>: Borrow<[u8]>` lets the
        // hashbrown table hash the slice without first allocating an
        // owned key. Only the first occurrence of each unique k-mer
        // pays the `to_vec()` cost.
        if let Some(v) = inner.get_mut(key_slice) {
            *v += by;
            return *v;
        }
        inner.insert(key_slice.to_vec().into_boxed_slice(), by);
        map.len_cache += 1;
        by
    })
}

/// `m.inc(key, by)` for `HashMap<String, i64>` — adds `by`
/// (default 1 in user code) to the value at `key`, inserting
/// the entry if absent. Halves the lock + hash work compared to
/// `m.insert(k, m.get_or(k, 0) + by)` and avoids the
/// double-borrow that pattern triggers in compiled mode.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_inc_str_i64(
    m: *mut GosMap,
    key: *const c_char,
    by: i64,
) -> i64 {
    ffi_entry!(-1, {
        if m.is_null() || key.is_null() {
            return 0;
        }
        let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes();
        let map = unsafe { &mut *m };
        let mut storage = map.storage.lock();
        if matches!(*storage, MapStorage::Empty) {
            *storage = MapStorage::StrI64(FxHashMap::default());
        }
        let MapStorage::StrI64(inner) = &mut *storage else {
            return 0;
        };
        if let Some(v) = inner.get_mut(key_bytes) {
            *v += by;
            return *v;
        }
        inner.insert(key_bytes.to_vec().into_boxed_slice(), by);
        map.len_cache += 1;
        by
    })
}

/// `m.or_insert(key, default)` — inserts `default` for `key` only when
/// the key is absent; returns the current (possibly just-inserted) value.
/// `HashMap<String, i64>` variant.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_or_insert_str_i64(
    m: *mut GosMap,
    key: *const c_char,
    default: i64,
) -> i64 {
    ffi_entry!(-1, {
        if m.is_null() || key.is_null() {
            return default;
        }
        let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes();
        let map = unsafe { &mut *m };
        let mut storage = map.storage.lock();
        if matches!(*storage, MapStorage::Empty) {
            *storage = MapStorage::StrI64(FxHashMap::default());
        }
        let MapStorage::StrI64(inner) = &mut *storage else {
            return default;
        };
        if let Some(v) = inner.get(key_bytes) {
            return *v;
        }
        inner.insert(key_bytes.to_vec().into_boxed_slice(), default);
        map.len_cache += 1;
        default
    })
}

/// `m.or_insert(key, default)` — `HashMap<i64, i64>` variant.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_or_insert_i64_i64(
    m: *mut GosMap,
    key: i64,
    default: i64,
) -> i64 {
    ffi_entry!(-1, {
        if m.is_null() {
            return default;
        }
        let map = unsafe { &mut *m };
        let mut storage = map.storage.lock();
        if matches!(*storage, MapStorage::Empty) {
            *storage = MapStorage::I64I64(FxHashMap::default());
        }
        let MapStorage::I64I64(inner) = &mut *storage else {
            return default;
        };
        if let Some(v) = inner.get(&key) {
            return *v;
        }
        inner.insert(key, default);
        map.len_cache += 1;
        default
    })
}

/// `m.insert(k: i64, v: String)` — `HashMap<i64, String>` insert.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_insert_i64_str(m: *mut GosMap, key: i64, val: *const c_char) {
    ffi_entry!((), {
        if m.is_null() || val.is_null() {
            return;
        }
        let map = unsafe { &mut *m };
        let val_bytes = unsafe { CStr::from_ptr(val) }.to_bytes().to_vec();
        let mut storage = map.storage.lock();
        if matches!(*storage, MapStorage::Empty) {
            *storage = MapStorage::I64Str(FxHashMap::default());
        }
        let MapStorage::I64Str(inner) = &mut *storage else {
            return;
        };
        if inner.insert(key, val_bytes.into_boxed_slice()).is_none() {
            map.len_cache += 1;
        }
        drop(storage);
        // Consuming insert copied the value bytes; release the moved-in gos-string.
        unsafe { gos_rt_str_free(val.cast_mut()) };
    });
}

/// `m.get(k: i64) -> String` — returns an empty string when absent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_i64_str(m: *const GosMap, key: i64) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if m.is_null() {
            return empty_cstring();
        }
        let map = unsafe { &*m };
        let storage = map.storage.lock();
        let MapStorage::I64Str(inner) = &*storage else {
            return empty_cstring();
        };
        match inner.get(&key) {
            Some(v) => alloc_cstring(v),
            None => empty_cstring(),
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_clear(m: *mut GosMap) {
    ffi_entry!((), {
        if m.is_null() {
            return;
        }
        let map = unsafe { &mut *m };
        let mut storage = map.storage.lock();
        *storage = MapStorage::Empty;
        map.len_cache = 0;
    });
}

/// Drops a `HashMap` allocated by [`gos_rt_map_new`] /
/// [`gos_rt_map_new_with_capacity`]. The MIR's drop-insertion pass
/// emits a call to this at every function return for any local
/// that owns a freshly-constructed map and isn't moved into the
/// return slot. Idempotent on null.
///
/// SAFETY: only call this on a pointer returned by one of the
/// runtime's `gos_rt_map_new*` constructors — the runtime's
/// [`GosMap`] layout includes a `parking_lot::Mutex<...>` and
/// dropping a binding-side `BindingGosMap` (two parallel `GosVec`
/// pointers) here would `Box::from_raw` the wrong shape and run
/// `Mutex::drop` over garbage. Use [`gos_rt_binding_map_free`] for
/// the binding-shaped aggregate instead.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_free(m: *mut GosMap) {
    ffi_entry!((), {
        if m.is_null() {
            return;
        }
        crate::c_abi::ledger::map_dec();
        drop(unsafe { Box::from_raw(m) });
    });
}

/// Wire shape of `gossamer_binding::native::BindingGosMap`. Defined
/// here (as a private type) so the dedicated free helper can box it
/// back without the runtime depending on the binding crate. The two
/// fields are pointers to `GosVec`-headed parallel arrays; the
/// binding crate's `make_gos_map` constructs both with
/// `Box::into_raw(Box::new(...))` so the matching free path walks
/// the same `Box`-shaped allocation.
#[repr(C)]
struct BindingGosMapLayout {
    keys: *mut GosVec,
    values: *mut GosVec,
}

/// Drops a binding-side map (a `BindingGosMap` from
/// `gossamer-binding`'s `native.rs`). Walks the two inner `GosVec`
/// pointers, freeing each via [`gos_rt_vec_free`], then drops the
/// outer `Box<BindingGosMapLayout>` allocation. Idempotent on null.
///
/// This is intentionally a separate symbol from [`gos_rt_map_free`]
/// because the two structs share a name across crates but have
/// incompatible layouts (one wraps a `parking_lot::Mutex<...>`, the
/// other is two raw pointers). Sending a binding-side pointer
/// through `gos_rt_map_free` would drop a `Mutex` over uninitialised
/// bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_binding_map_free(m: *mut u8) {
    ffi_entry!((), {
        if m.is_null() {
            return;
        }
        let boxed = unsafe { Box::from_raw(m.cast::<BindingGosMapLayout>()) };
        unsafe {
            gos_rt_vec_free(boxed.keys);
            gos_rt_vec_free(boxed.values);
        }
        drop(boxed);
    });
}

/// Drops a `Vec` allocated by [`gos_rt_vec_new`] /
/// [`gos_rt_vec_with_capacity`] / [`gos_rt_vec_new_typed`]. Frees
/// the `GosVec` header, the backing element buffer, and — when
/// `elem_kind != PRIMITIVE` — every pointer-bearing element
/// payload (cstring, nested Vec, Map, Error). Idempotent on null.
///
/// The default `elem_kind = PRIMITIVE` path matches pre-0.6
/// behaviour: shallow free of the byte buffer. Typed vecs created
/// via `gos_rt_vec_new_typed` opt in to deep free.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_free(v: *mut GosVec) {
    ffi_entry!((), {
        if v.is_null() {
            return;
        }
        // Region-allocated vecs (header + buffer in arena slabs) are freed
        // wholesale at `region_pop` — never individually. Touching them here
        // via `Box::from_raw` / `Vec::from_raw_parts` would corrupt the
        // global allocator (the memory isn't its).
        if crate::c_abi::vec::vec_is_region(unsafe { &*v }) {
            return;
        }
        // RC: this is a release. Decrement; reclaim only when the last
        // reference drops. An aliased Vec (`let b = v`) still has live holders.
        let rc = crate::c_abi::vec::vec_rc(unsafe { &*v });
        if rc > 1 {
            crate::c_abi::vec::vec_set_rc(unsafe { &mut *v }, rc - 1);
            return;
        }
        crate::c_abi::ledger::vec_dec();
        let boxed = unsafe { Box::from_raw(v) };
        if !boxed.ptr.is_null() && boxed.cap > 0 {
            // Deep-free pointer-bearing element payloads BEFORE
            // reclaiming the backing buffer. Each branch walks the
            // first `len` slots — slots between `len` and `cap` were
            // never written and contain the zero-init produced by
            // `vec![0u8; bytes]` at construction time.
            if boxed.elem_kind != vec_elem_kind::PRIMITIVE && boxed.elem_bytes as usize == 8 {
                let count = boxed.len.max(0) as usize;
                // SAFETY: ptr is non-null + cap > 0 (checked above);
                // we only read `count <= len <= cap` slots of 8 bytes
                // each, all initialised by construction.
                let slots =
                    unsafe { std::slice::from_raw_parts(boxed.ptr.cast::<*mut u8>(), count) };
                for &slot in slots {
                    if slot.is_null() {
                        continue;
                    }
                    match boxed.elem_kind {
                        vec_elem_kind::STRING => {
                            // SAFETY: each slot in a STRING-typed vec was
                            // populated via gos_rt_str_clone / alloc_cstring
                            // and therefore carries the allocator tag.
                            unsafe { gos_rt_str_free(slot.cast::<c_char>()) };
                        }
                        vec_elem_kind::VEC => {
                            unsafe { gos_rt_vec_free(slot.cast::<GosVec>()) };
                        }
                        vec_elem_kind::MAP => {
                            unsafe { gos_rt_map_free(slot.cast::<GosMap>()) };
                        }
                        vec_elem_kind::ERROR => {
                            // No dedicated free helper yet; drop the
                            // raw Box (allocated via `Box::into_raw`
                            // elsewhere in the file). Safe because
                            // `GosError`'s own drop chains through the
                            // message + cause heap allocations.
                            let _ = unsafe { Box::from_raw(slot.cast::<GosError>()) };
                        }
                        _ => {}
                    }
                }
            }
            let bytes = (boxed.cap as usize) * (boxed.elem_bytes as usize);
            unsafe {
                let _ = Vec::from_raw_parts(boxed.ptr.as_ptr(), bytes, bytes);
            }
        }
        drop(boxed);
    });
}

/// Drops a `HashSet` allocated by [`gos_rt_set_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_free(s: *mut GosSet) {
    ffi_entry!((), {
        if s.is_null() {
            return;
        }
        drop(unsafe { Box::from_raw(s) });
    });
}

/// Drops a `BTreeMap` allocated by [`gos_rt_btmap_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_btmap_free(m: *mut GosBtMap) {
    ffi_entry!((), {
        if m.is_null() {
            return;
        }
        drop(unsafe { Box::from_raw(m) });
    });
}

/// Snapshots the i64 keys of an i64-keyed `HashMap` into a fresh
/// `GosVec<i64>` for the for-loop lowerer to drive with the
/// regular `gos_rt_vec_*` helpers. Iteration order matches the
/// underlying `FxHashMap`'s order — undefined-but-stable per
/// process. Returns an empty vec for any other storage shape.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_keys_i64(m: *const GosMap) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        if m.is_null() {
            return out;
        }
        let map = unsafe { &*m };
        let storage = map.storage.lock();
        let push_key = |k: &i64| {
            let bytes = k.to_ne_bytes();
            unsafe { gos_rt_vec_push(out, bytes.as_ptr()) };
        };
        match &*storage {
            MapStorage::I64I64(inner) => inner.keys().for_each(push_key),
            MapStorage::I64Str(inner) => inner.keys().for_each(push_key),
            _ => {}
        }
        out
    })
}

/// Snapshots the i64 values of an i64-valued `HashMap` into a
/// fresh `GosVec<i64>`. Pairs with `gos_rt_map_keys_i64` for
/// `for v in m.values()` lowering. Empty vec for non-i64-valued
/// storage shapes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_values_i64(m: *const GosMap) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        if m.is_null() {
            return out;
        }
        let map = unsafe { &*m };
        let storage = map.storage.lock();
        match &*storage {
            MapStorage::I64I64(inner) => {
                for v in inner.values() {
                    let bytes = v.to_ne_bytes();
                    unsafe { gos_rt_vec_push(out, bytes.as_ptr()) };
                }
            }
            MapStorage::StrI64(inner) => {
                for v in inner.values() {
                    let bytes = v.to_ne_bytes();
                    unsafe { gos_rt_vec_push(out, bytes.as_ptr()) };
                }
            }
            _ => {}
        }
        out
    })
}

/// Snapshots the string keys of a string-keyed `HashMap` into a
/// fresh `GosVec<*mut c_char>`. Each key is freshly allocated in
/// the GC arena so the slot value is the same `*mut c_char`
/// representation Gossamer's `String` type uses elsewhere.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_keys_str(m: *const GosMap) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        if m.is_null() {
            return out;
        }
        let map = unsafe { &*m };
        let storage = map.storage.lock();
        let push_key = |k: &[u8]| {
            let cstr = alloc_cstring(k);
            let slot = (cstr as usize as i64).to_ne_bytes();
            unsafe { gos_rt_vec_push(out, slot.as_ptr()) };
        };
        match &*storage {
            MapStorage::StrI64(inner) => {
                for k in inner.keys() {
                    push_key(k);
                }
            }
            MapStorage::StrStr(inner) => {
                for k in inner.keys() {
                    push_key(k);
                }
            }
            _ => {}
        }
        out
    })
}

/// Snapshots the string values of a string-valued `HashMap` into
/// a fresh `GosVec<*mut c_char>`. Mirrors `gos_rt_map_keys_str`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_values_str(m: *const GosMap) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe { gos_rt_vec_new(8) };
        if m.is_null() {
            return out;
        }
        let map = unsafe { &*m };
        let storage = map.storage.lock();
        let push_val = |v: &[u8]| {
            let cstr = alloc_cstring(v);
            let slot = (cstr as usize as i64).to_ne_bytes();
            unsafe { gos_rt_vec_push(out, slot.as_ptr()) };
        };
        match &*storage {
            MapStorage::StrStr(inner) => inner.values().for_each(|v| push_val(v)),
            MapStorage::I64Str(inner) => inner.values().for_each(|v| push_val(v)),
            _ => {}
        }
        out
    })
}

fn empty_cstring() -> *mut c_char {
    alloc_cstring(b"")
}

/// Auto-dispatch `m.keys() -> Vec<K>` based on the live map storage.
/// I64-keyed maps return a `Vec<i64>`; string-keyed maps return a
/// `Vec<*mut c_char>`. Empty Vec for empty / unknown storage shapes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_keys_vec(m: *const GosMap) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        if m.is_null() {
            return unsafe { gos_rt_vec_new(8) };
        }
        let map = unsafe { &*m };
        let storage = map.storage.lock();
        match &*storage {
            MapStorage::I64I64(_) | MapStorage::I64Str(_) => {
                drop(storage);
                unsafe { gos_rt_map_keys_i64(m) }
            }
            MapStorage::StrI64(_) | MapStorage::StrStr(_) | MapStorage::Bytes(_) => {
                drop(storage);
                unsafe { gos_rt_map_keys_str(m) }
            }
            // Struct keys are stored as flat content bytes; rebuilding the
            // aggregate values would need the key's layout, which isn't
            // threaded here. `keys()` over a struct-keyed map is unsupported.
            MapStorage::SkeyVal(_) | MapStorage::Empty => unsafe { gos_rt_vec_new(8) },
        }
    })
}

/// Auto-dispatch `m.values() -> Vec<V>` based on the live map
/// storage. Mirrors [`gos_rt_map_keys_vec`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_values_vec(m: *const GosMap) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        if m.is_null() {
            return unsafe { gos_rt_vec_new(8) };
        }
        let map = unsafe { &*m };
        let storage = map.storage.lock();
        match &*storage {
            MapStorage::I64I64(_) | MapStorage::StrI64(_) => {
                drop(storage);
                unsafe { gos_rt_map_values_i64(m) }
            }
            MapStorage::StrStr(_) | MapStorage::I64Str(_) | MapStorage::Bytes(_) => {
                drop(storage);
                unsafe { gos_rt_map_values_str(m) }
            }
            MapStorage::SkeyVal(_) | MapStorage::Empty => unsafe { gos_rt_vec_new(8) },
        }
    })
}

/// `m.pop(k) -> Option<V>` for an i64-keyed map. Removes the entry
/// at `k` if present and returns the previous value as Some;
/// returns None otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_pop_i64(m: *mut GosMap, key: i64) -> i128 {
    ffi_entry!(0i128, {
        if m.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let map = unsafe { &mut *m };
        let mut storage = map.storage.lock();
        let popped: Option<i64> = match &mut *storage {
            MapStorage::I64I64(inner) => inner.remove(&key),
            MapStorage::I64Str(inner) => inner.remove(&key).map(|bs| {
                let cstr = alloc_cstring(&bs);
                cstr as i64
            }),
            _ => None,
        };
        if popped.is_some() {
            map.len_cache = map.len_cache.saturating_sub(1);
        }
        match popped {
            Some(v) => unsafe { gos_rt_result_new(0, v) },
            None => unsafe { gos_rt_result_new(1, 0) },
        }
    })
}

/// `m.pop(k) -> Option<V>` for a string-keyed map. The key is a
/// c-string pointer; the returned Option payload is the
/// raw 8-byte previous value (i64 directly for `StrI64`,
/// `*mut c_char` cast to i64 for `StrStr`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_pop_str(m: *mut GosMap, key: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if m.is_null() || key.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let map = unsafe { &mut *m };
        let key_bytes = unsafe { CStr::from_ptr(key).to_bytes() };
        let mut storage = map.storage.lock();
        let popped: Option<i64> = match &mut *storage {
            MapStorage::StrI64(inner) => inner.remove(key_bytes),
            MapStorage::StrStr(inner) | MapStorage::Bytes(inner) => {
                inner.remove(key_bytes).map(|bs| {
                    let cstr = alloc_cstring(&bs);
                    cstr as i64
                })
            }
            _ => None,
        };
        if popped.is_some() {
            map.len_cache = map.len_cache.saturating_sub(1);
        }
        match popped {
            Some(v) => unsafe { gos_rt_result_new(0, v) },
            None => unsafe { gos_rt_result_new(1, 0) },
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_remove(m: *mut GosMap, key: *const u8) -> i32 {
    ffi_entry!(-1, {
        if m.is_null() || key.is_null() {
            return 0;
        }
        let map = unsafe { &mut *m };
        let k = unsafe { std::slice::from_raw_parts(key, 8) };
        let mut storage = map.storage.lock();
        let removed = match &mut *storage {
            MapStorage::Bytes(inner) => inner.remove(k).is_some(),
            _ => false,
        };
        if removed {
            map.len_cache -= 1;
            1
        } else {
            0
        }
    })
}

#[cfg(test)]
mod map_iter_tests {
    use super::*;

    #[test]
    fn map_keys_i64_snapshots_inserted_keys() {
        unsafe {
            let m = gos_rt_map_new(8, 8);
            gos_rt_map_insert_i64_i64(m, 1, 100);
            gos_rt_map_insert_i64_i64(m, 2, 200);
            gos_rt_map_insert_i64_i64(m, 3, 50);
            assert_eq!(gos_rt_map_len(m), 3);
            let v = gos_rt_map_keys_i64(m);
            assert_eq!(gos_rt_vec_len(v), 3);
            let mut keys: Vec<i64> = (0..gos_rt_vec_len(v))
                .map(|i| {
                    let p = gos_rt_vec_get_ptr(v, i);
                    (p as *const i64).read_unaligned()
                })
                .collect();
            keys.sort_unstable();
            assert_eq!(keys, vec![1, 2, 3]);
        }
    }
}
