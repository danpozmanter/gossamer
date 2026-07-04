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
// HashMap runtime - typed-storage variants over rustc-hash's
// FxHashMap. Auto-promotes Empty → I64I64 / StrI64 / StrStr /
// Bytes on first typed call. The i64-keyed/i64-valued shape
// (counter / scoreboard hot paths) avoids per-op `Vec<u8>`
// allocation and uses FxHash directly on the
// 8-byte key.
// ---------------------------------------------------------------

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};

use rustc_hash::FxHashMap;

/// A mutex whose lock is *biased* to the goroutine that owns the map:
/// while the map has not escaped to another goroutine it is accessed
/// with no locking at all, and only an escaped (shared) map pays the
/// `parking_lot` acquire/release on each operation.
///
/// Gossamer's model is "share by communicating": an ordinary
/// `HashMap` lives on one goroutine and is never touched concurrently,
/// so the per-operation lock the unconditional mutex imposed was pure
/// overhead on the common path (the k-mer-counter hot loop pays it tens
/// of millions of times). A map only becomes reachable from a second
/// goroutine through an explicit escape point - captured by a `go` /
/// `spawn` closure, or sent on a channel - and the codegen marks it
/// `shared` *on the owning goroutine, before the value is published*
/// (the same protocol `RcHeader` / string values use via
/// `gos_rt_rc_mark_shared`). After that flip every operation locks, so
/// concurrent access to a genuinely shared map is fully synchronized -
/// strictly safer than Go's unsynchronized maps, with zero cost when no
/// sharing exists.
struct BiasedLock<T> {
    shared: AtomicBool,
    inner: parking_lot::Mutex<T>,
}

impl<T> BiasedLock<T> {
    fn new(value: T) -> Self {
        BiasedLock {
            shared: AtomicBool::new(false),
            inner: parking_lot::Mutex::new(value),
        }
    }

    /// Acquires access to the protected value. Lock-free for a
    /// goroutine-local map; takes the real lock once the map is shared.
    #[inline]
    fn lock(&self) -> BiasedGuard<'_, T> {
        if self.shared.load(Ordering::Acquire) {
            BiasedGuard::Locked(self.inner.lock())
        } else {
            // The map is owned by a single goroutine, so no other thread
            // can touch `inner` for the duration of this borrow. The flip
            // to `shared` happens on this goroutine before the map is
            // published to any other (see the type doc), so the load
            // above cannot miss an escape that another thread could race.
            BiasedGuard::Local(unsafe { &mut *self.inner.data_ptr() })
        }
    }

    /// Marks the map shared so every subsequent operation synchronizes.
    /// Called on the owning goroutine before the map escapes; idempotent.
    #[inline]
    fn mark_shared(&self) {
        self.shared.store(true, Ordering::Release);
    }
}

/// Access handle from [`BiasedLock::lock`]. Derefs to the protected
/// value in both the lock-free and the locked case; the `Locked`
/// variant releases the mutex on drop.
enum BiasedGuard<'a, T> {
    Local(&'a mut T),
    Locked(parking_lot::MutexGuard<'a, T>),
}

impl<T> Deref for BiasedGuard<'_, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        match self {
            BiasedGuard::Local(r) => r,
            BiasedGuard::Locked(g) => g,
        }
    }
}

impl<T> DerefMut for BiasedGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        match self {
            BiasedGuard::Local(r) => r,
            BiasedGuard::Locked(g) => g,
        }
    }
}

/// Layout-sensitive: the first 8 bytes hold the current element
/// count so the generic `gos_rt_arr_len` returns the right value
/// without needing a HashMap-specific dispatch.
#[repr(C)]
pub struct GosMap {
    len_cache: i64,
    storage: BiasedLock<MapStorage>,
    /// Values are RC copy-blobs (`gos_rt_rc_alloc_copy` results): the
    /// map owns one share per stored value. Inserts release the
    /// overwritten value, removals and `gos_rt_map_free` release the
    /// stored ones, and the `_opt` getters retain before handing the
    /// pointer out (the receiving option holder releases it). Set by
    /// `gos_rt_map_set_blob_values` right after construction when the
    /// declared value type is a guarded aggregate; appended last so
    /// existing field offsets are unchanged.
    blob_values: std::sync::atomic::AtomicBool,
}

enum MapStorage {
    Empty,
    I64I64(FxHashMap<i64, i64>),
    /// String-keyed maps store keys as `Box<[u8]>` (16 B header)
    /// rather than `Vec<u8>` (24 B header) - for the k-mer-counter
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
    /// compare equal, matching the VM), the value is an 8-byte word - an
    /// `i64`, or a heap pointer for `String` / struct values.
    SkeyVal(FxHashMap<Box<[u8]>, i64>),
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_new(_key_bytes: u32, _val_bytes: u32) -> *mut GosMap {
    ffi_entry!(std::ptr::null_mut(), {
        crate::c_abi::ledger::map_inc();
        Box::into_raw(Box::new(GosMap {
            len_cache: 0,
            storage: BiasedLock::new(MapStorage::Empty),
            blob_values: std::sync::atomic::AtomicBool::new(false),
        }))
    })
}

/// Pre-sized constructor: avoids the doubling chain (~22 reallocs
/// for ~5M inserts) when the caller has an upper bound. Picks the
/// initial typed shape from the byte sizes - both 8 → I64I64,
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
            storage: BiasedLock::new(storage),
            blob_values: std::sync::atomic::AtomicBool::new(false),
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
        let key_bytes = unsafe { crate::c_abi::string::gos_str_key_bytes(key) };
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
            unsafe { crate::c_abi::string::gos_str_key_bytes(default) }
        };
        if m.is_null() || key.is_null() {
            return alloc_cstring(default_bytes);
        }
        let map = unsafe { &*m };
        let key_bytes = unsafe { crate::c_abi::string::gos_str_key_bytes(key) };
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
            unsafe { crate::c_abi::string::gos_str_key_bytes(default) }
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
        let prev = inner.insert(key, val);
        if prev.is_none() {
            map.len_cache += 1;
        }
        if map_has_blob_values(map)
            && let Some(old) = prev
            && old != val
        {
            // Overwriting a copy-blob value: the map's share of the old
            // one is released (set-gated in the RC layer).
            unsafe { release_blob_value(old) };
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
                // The string field holds a cstring pointer exposed as an
                // integer by the flat-slot ABI; recover its provenance.
                let raw = unsafe { (slot as *const usize).read_unaligned() };
                let sptr: *const c_char = std::ptr::with_exposed_provenance(raw);
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
        let prev = inner.insert(k.into_boxed_slice(), val);
        if prev.is_none() {
            map.len_cache += 1;
        }
        if map_has_blob_values(map)
            && let Some(old) = prev
            && old != val
        {
            unsafe { release_blob_value(old) };
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
        if let Some(v) = payload
            && map_has_blob_values(map)
        {
            unsafe { retain_blob_value(v) };
        }
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
            Some(v) => {
                // Blob values: the caller's option holder receives (and
                // later releases) its own share; the map keeps its own.
                if map_has_blob_values(map) {
                    unsafe { retain_blob_value(v) };
                }
                unsafe { gos_rt_result_new(0, v) }
            }
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
        let blob_values = map_has_blob_values(map);
        let removed = match &mut *storage {
            MapStorage::I64I64(inner) => match inner.remove(&key) {
                Some(old) => {
                    if blob_values {
                        unsafe { release_blob_value(old) };
                    }
                    true
                }
                None => false,
            },
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
        let key_bytes = unsafe { crate::c_abi::string::gos_str_key_bytes(key) };
        let mut storage = map.storage.lock();
        if matches!(*storage, MapStorage::Empty) {
            *storage = MapStorage::StrI64(FxHashMap::default());
        }
        let MapStorage::StrI64(inner) = &mut *storage else {
            return;
        };
        let prev = inner.insert(crate::c_abi::string::boxed_bytes(key_bytes), val);
        if prev.is_none() {
            map.len_cache += 1;
        }
        // Overwriting a copy-blob value (e.g. a `Vec<i64>` handle in a
        // `HashMap<String, Vec<i64>>`): release the map's share of the
        // old word, mirroring the i64/i64 insert path. Gated on the
        // blob-values flag so scalar-valued maps stay untouched.
        let release_old = if map_has_blob_values(map) && prev != Some(val) {
            prev
        } else {
            None
        };
        drop(storage);
        if let Some(old) = release_old {
            unsafe { release_blob_value(old) };
        }
        // Consuming insert copied the key bytes; release the moved-in gos-string
        // (rc-aware + tag-checked - safe for temps, shared, and literals).
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
        let key_bytes = unsafe { crate::c_abi::string::gos_str_key_bytes(key) };
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
        let key_bytes = unsafe { crate::c_abi::string::gos_str_key_bytes(key) };
        let storage = map.storage.lock();
        let payload: Option<i64> = match &*storage {
            MapStorage::StrI64(inner) => inner.get(key_bytes).copied(),
            MapStorage::StrStr(inner) | MapStorage::Bytes(inner) => {
                inner.get(key_bytes).map(|bs| alloc_cstring(bs) as i64)
            }
            _ => None,
        };
        // Handing out a copy-blob value (Vec / struct handle) shares
        // ownership with the caller: retain so the map's later drop
        // and the caller's drop are balanced. Gated like the i64/i64
        // get path; the StrStr/Bytes arms allocate a fresh c-string
        // and are not blob-values.
        if let Some(v) = payload
            && matches!(&*storage, MapStorage::StrI64(_))
            && map_has_blob_values(map)
        {
            unsafe { retain_blob_value(v) };
        }
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
        let key_bytes = unsafe { crate::c_abi::string::gos_str_key_bytes(key) };
        let val_bytes = unsafe { crate::c_abi::string::gos_str_key_bytes(val) };
        let mut storage = map.storage.lock();
        if matches!(*storage, MapStorage::Empty) {
            *storage = MapStorage::StrStr(FxHashMap::default());
        }
        let MapStorage::StrStr(inner) = &mut *storage else {
            return;
        };
        if inner
            .insert(
                crate::c_abi::string::boxed_bytes(key_bytes),
                crate::c_abi::string::boxed_bytes(val_bytes),
            )
            .is_none()
        {
            map.len_cache += 1;
        }
        drop(storage);
        // The map copied the key/val bytes into its own storage, so it does not
        // retain the inbound gos-strings. `map_insert` is a consuming call (the
        // drop pass moves its arguments in), so release the originals here -
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
        let key_bytes = unsafe { crate::c_abi::string::gos_str_key_bytes(key) };
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
        let key_bytes = unsafe { crate::c_abi::string::gos_str_key_bytes(key) };
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
        let key_bytes = unsafe { crate::c_abi::string::gos_str_key_bytes(key) };
        let mut storage = map.storage.lock();
        let blob_values = map_has_blob_values(map);
        let removed = match &mut *storage {
            MapStorage::StrI64(inner) => match inner.remove(key_bytes) {
                Some(old) => {
                    if blob_values {
                        unsafe { release_blob_value(old) };
                    }
                    true
                }
                None => false,
            },
            MapStorage::StrStr(inner) => inner.remove(key_bytes).is_some(),
            _ => false,
        };
        if removed {
            map.len_cache -= 1;
        }
        removed
    })
}

/// `m.inc_at(seq, start, len, by)` for `HashMap<String, i64>` -
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
        // The true sequence length is read O(1) from the string's length
        // header (every runtime-built string carries one; a foreign pointer
        // falls back to `strlen`), the same source `gos_rt_map_inc_str_i64`
        // uses. Reject a window that runs past the sequence and validate its
        // UTF-8 before slicing, mirroring the interpreter builtin so an
        // out-of-range or non-boundary `inc_at` yields 0 on every tier
        // instead of reading adjacent heap.
        let seq_bytes = unsafe { crate::c_abi::string::gos_str_key_bytes(seq) };
        let (start_u, len_u) = (start as usize, len as usize);
        let end_u = match start_u.checked_add(len_u) {
            Some(end) if end <= seq_bytes.len() => end,
            _ => return 0,
        };
        let key_slice = &seq_bytes[start_u..end_u];
        if std::str::from_utf8(key_slice).is_err() {
            return 0;
        }
        let map = unsafe { &mut *m };
        let mut storage = map.storage.lock();
        if matches!(*storage, MapStorage::Empty) {
            *storage = MapStorage::StrI64(FxHashMap::default());
        }
        let MapStorage::StrI64(inner) = &mut *storage else {
            return 0;
        };
        // Lookup is by `&[u8]` - `Vec<u8>: Borrow<[u8]>` lets the
        // hashbrown table hash the slice without first allocating an
        // owned key. Only the first occurrence of each unique k-mer
        // pays the `to_vec()` cost.
        if let Some(v) = inner.get_mut(key_slice) {
            *v += by;
            return *v;
        }
        inner.insert(crate::c_abi::string::boxed_bytes(key_slice), by);
        map.len_cache += 1;
        by
    })
}

/// `m.inc(key, by)` for `HashMap<String, i64>` - adds `by`
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
        let key_bytes = unsafe { crate::c_abi::string::gos_str_key_bytes(key) };
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
        inner.insert(crate::c_abi::string::boxed_bytes(key_bytes), by);
        map.len_cache += 1;
        by
    })
}

/// `m.or_insert(key, default)` - inserts `default` for `key` only when
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
        let key_bytes = unsafe { crate::c_abi::string::gos_str_key_bytes(key) };
        let map = unsafe { &mut *m };
        let mut storage = map.storage.lock();
        if matches!(*storage, MapStorage::Empty) {
            *storage = MapStorage::StrI64(FxHashMap::default());
        }
        let MapStorage::StrI64(inner) = &mut *storage else {
            return default;
        };
        if let Some(v) = inner.get(key_bytes).copied() {
            // Key present: hand back the stored value. For a copy-blob
            // value (Vec / struct handle) this is a shared hand-out, so
            // retain to balance the caller's later drop of the returned
            // value (mirrors gos_rt_map_get_str_opt). The unused
            // `default` blob is released here, since or_insert owns it.
            if map_has_blob_values(map) {
                unsafe { retain_blob_value(v) };
                if default != v {
                    unsafe { release_blob_value(default) };
                }
            }
            return v;
        }
        // Key absent: store `default`. Aggregate (blob) values inserted
        // here on a previously-absent key currently hang the compiled
        // tier at teardown (a deeper RC-ownership issue between the
        // coerced literal, the stored word, and the returned word); the
        // scalar path and the key-present path are unaffected. The
        // aggregate_binding fixture exercises the working shapes.
        inner.insert(crate::c_abi::string::boxed_bytes(key_bytes), default);
        map.len_cache += 1;
        default
    })
}

/// `m.or_insert(key, default)` - `HashMap<i64, i64>` variant.
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

/// `m.insert(k: i64, v: String)` - `HashMap<i64, String>` insert.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_insert_i64_str(m: *mut GosMap, key: i64, val: *const c_char) {
    ffi_entry!((), {
        if m.is_null() || val.is_null() {
            return;
        }
        let map = unsafe { &mut *m };
        let val_bytes = unsafe { crate::c_abi::string::gos_str_key_bytes(val) };
        let mut storage = map.storage.lock();
        if matches!(*storage, MapStorage::Empty) {
            *storage = MapStorage::I64Str(FxHashMap::default());
        }
        let MapStorage::I64Str(inner) = &mut *storage else {
            return;
        };
        if inner
            .insert(key, crate::c_abi::string::boxed_bytes(val_bytes))
            .is_none()
        {
            map.len_cache += 1;
        }
        drop(storage);
        // Consuming insert copied the value bytes; release the moved-in gos-string.
        unsafe { gos_rt_str_free(val.cast_mut()) };
    });
}

/// `m.get(k: i64) -> String` - returns an empty string when absent.
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
        if map_has_blob_values(map) {
            match &*storage {
                MapStorage::I64I64(inner) => {
                    for &v in inner.values() {
                        unsafe { release_blob_value(v) };
                    }
                }
                MapStorage::StrI64(inner) | MapStorage::SkeyVal(inner) => {
                    for &v in inner.values() {
                        unsafe { release_blob_value(v) };
                    }
                }
                _ => {}
            }
        }
        *storage = MapStorage::Empty;
        map.len_cache = 0;
    });
}

/// Renders a tuple's flat slot buffer to `(a, b, …)` (a 1-tuple
/// gets a trailing comma, `(a,)`), matching the VM's `Display`.
/// `p` points at `n` contiguous 8-byte slots; `tags[i]` selects how
/// slot `i` is interpreted: `0` = Int, `2` = Float (the slot's bits
/// are an `f64`), `3` = Bool (low bit), `4` = Char (low 32 bits as a
/// code point), `5` = Str (the slot is a c-string pointer). Integers
/// and floats route through `crate::builtins::format_int` /
/// `format_float` so the rendering is byte-identical to the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_tuple_format(
    p: *const i64,
    n: i64,
    tags: *const u8,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if p.is_null() || tags.is_null() || n <= 0 {
            return alloc_cstring(b"()");
        }
        let n = n as usize;
        let mut out = String::from("(");
        for i in 0..n {
            if i > 0 {
                out.push_str(", ");
            }
            let word = unsafe { p.add(i).read_unaligned() };
            let tag = unsafe { *tags.add(i) };
            match tag {
                0 => out.push_str(&crate::builtins::format_int(word)),
                2 => out.push_str(&crate::builtins::format_float(f64::from_bits(word as u64))),
                3 => out.push_str(crate::builtins::format_bool(word & 1 != 0)),
                4 => {
                    if let Some(c) = char::from_u32(word as u32) {
                        out.push(c);
                    }
                }
                5 => {
                    let sp: *const c_char = std::ptr::with_exposed_provenance(word as usize);
                    if !sp.is_null() {
                        out.push_str(&unsafe { CStr::from_ptr(sp) }.to_string_lossy());
                    }
                }
                _ => {}
            }
        }
        if n == 1 {
            out.push(',');
        }
        out.push(')');
        alloc_cstring(out.as_bytes())
    })
}

/// Lexicographically compares two tuples' flat slot buffers, returning
/// `-1` / `0` / `1`. `a` and `b` point at `n` contiguous 8-byte slots;
/// `tags[i]` selects each slot's kind (same encoding as
/// [`gos_rt_tuple_format`]: `0` Int, `2` Float, `3` Bool, `4` Char, `5`
/// Str). The first non-equal element decides; equal prefixes continue.
/// Routed to by the compiled tiers for tuple `== != < <= > >=`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_tuple_cmp(
    a: *const i64,
    b: *const i64,
    n: i64,
    tags: *const u8,
) -> i64 {
    ffi_entry!(0, {
        use std::cmp::Ordering;
        if a.is_null() || b.is_null() || tags.is_null() || n <= 0 {
            return 0;
        }
        let n = n as usize;
        for i in 0..n {
            let wa = unsafe { a.add(i).read_unaligned() };
            let wb = unsafe { b.add(i).read_unaligned() };
            let ord = match unsafe { *tags.add(i) } {
                2 => f64::from_bits(wa as u64)
                    .partial_cmp(&f64::from_bits(wb as u64))
                    .unwrap_or(Ordering::Equal),
                3 => (wa & 1).cmp(&(wb & 1)),
                4 => (wa as u32).cmp(&(wb as u32)),
                5 => {
                    let sa: *const c_char = std::ptr::with_exposed_provenance(wa as usize);
                    let sb: *const c_char = std::ptr::with_exposed_provenance(wb as usize);
                    unsafe { gos_rt_str_compare(sa, sb) }.cmp(&0)
                }
                _ => wa.cmp(&wb),
            };
            match ord {
                Ordering::Less => return -1,
                Ordering::Greater => return 1,
                Ordering::Equal => {}
            }
        }
        0
    })
}

/// Structural equality of two Vec/array values. `elem_tag` (same encoding
/// as [`gos_rt_tuple_cmp`]) selects how each element slot is interpreted:
/// `2` Float (bit-equal would mishandle NaN), `5` Str (per-element
/// `gos_rt_str_eq`), anything else a plain word compare. Routed to by the
/// compiled tiers for `[T] == [T]` / `!=`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_eq(a: *const GosVec, b: *const GosVec, elem_tag: u8) -> bool {
    ffi_entry!(false, {
        if a.is_null() || b.is_null() {
            return std::ptr::eq(a, b);
        }
        let la = unsafe { (*a).len };
        if la != unsafe { (*b).len } {
            return false;
        }
        for i in 0..la {
            let wa = unsafe { gos_rt_vec_get_i64(a, i) };
            let wb = unsafe { gos_rt_vec_get_i64(b, i) };
            let eq = match elem_tag {
                2 => f64::from_bits(wa as u64) == f64::from_bits(wb as u64),
                5 => {
                    let sa: *const c_char = std::ptr::with_exposed_provenance(wa as usize);
                    let sb: *const c_char = std::ptr::with_exposed_provenance(wb as usize);
                    unsafe { gos_rt_str_eq(sa, sb) }
                }
                _ => wa == wb,
            };
            if !eq {
                return false;
            }
        }
        true
    })
}

/// Structural equality of two heap (recursive / `Box`) enum nodes, driven by
/// a per-enum descriptor blob so equal-but-distinct allocations compare true
/// (matching the VM's `values_equal`) instead of by pointer identity. The
/// compiled tiers route heap-enum `==` / `!=` here.
///
/// `desc` (pure `i64`): `[num_variants]` then, per variant in discriminant
/// order, `[num_fields, kind_0, .., kind_{n-1}]`. Field kinds: `0` word
/// (int / bool / char), `1` `f64`, `2` `String`, `3` nested self-enum
/// (recurse with the same `desc`), `4` `Vec<self-enum>`, `5`
/// `Vec<(String, self-enum)>`. The codegen emits a descriptor - and routes
/// here - only when every nested enum field is the same type, so a mismatched
/// sub-shape never reaches this walk.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_enum_struct_eq(a: *mut u8, b: *mut u8, desc: *const i64) -> i64 {
    ffi_entry!(0, {
        let raw_a = a as usize;
        let raw_b = b as usize;
        let a = crate::c_abi::rc::untag_rc(a);
        let b = crate::c_abi::rc::untag_rc(b);
        if std::ptr::eq(a, b) {
            return 1; // same node, or both null
        }
        if a.is_null() || b.is_null() || desc.is_null() {
            return 0;
        }
        // Discriminant: a small heap enum tags it into the pointer's low bits
        // (`base | (disc << 1)`); a larger one stores it in the RcHeader byte at
        // payload-3. A zero tag means disc 0 or the header form - the header
        // then holds the value (0 for a tagged disc-0 node too, so both agree).
        let disc_of = |raw: usize, base: *mut u8| -> u8 {
            let tag = raw & 7;
            if tag != 0 {
                (tag >> 1) as u8
            } else {
                unsafe { *base.sub(3) }
            }
        };
        let da = disc_of(raw_a, a);
        let db = disc_of(raw_b, b);
        if da != db {
            return 0;
        }
        let num_variants = unsafe { *desc };
        if i64::from(da) >= num_variants {
            return 0;
        }
        let mut idx = 1usize;
        for _ in 0..da {
            let nf = unsafe { *desc.add(idx) }.max(0);
            idx += 1 + nf as usize;
        }
        let nf = unsafe { *desc.add(idx) }.max(0);
        idx += 1;
        for f in 0..nf {
            let kind = unsafe { *desc.add(idx + f as usize) };
            let wa = unsafe { *(a as *const i64).add(f as usize) };
            let wb = unsafe { *(b as *const i64).add(f as usize) };
            let eq = match kind {
                1 => f64::from_bits(wa as u64) == f64::from_bits(wb as u64),
                2 => {
                    let sa: *const c_char = std::ptr::with_exposed_provenance(wa as usize);
                    let sb: *const c_char = std::ptr::with_exposed_provenance(wb as usize);
                    unsafe { gos_rt_str_eq(sa, sb) }
                }
                3 => unsafe { gos_rt_enum_struct_eq(wa as *mut u8, wb as *mut u8, desc) != 0 },
                4 => unsafe { vec_self_enum_eq(wa, wb, desc) },
                5 => unsafe { vec_str_self_enum_eq(wa, wb, desc) },
                _ => wa == wb,
            };
            if !eq {
                return 0;
            }
        }
        1
    })
}

/// Element-wise structural equality of two `Vec<self-enum>` field words (each
/// a `*mut GosVec` of 8-byte enum-pointer slots), recursing per element.
unsafe fn vec_self_enum_eq(a_word: i64, b_word: i64, desc: *const i64) -> bool {
    let va = a_word as *const GosVec;
    let vb = b_word as *const GosVec;
    if std::ptr::eq(va, vb) {
        return true;
    }
    if va.is_null() || vb.is_null() {
        return false;
    }
    let la = unsafe { (*va).len };
    if la != unsafe { (*vb).len } {
        return false;
    }
    for i in 0..la {
        let ea = unsafe { gos_rt_vec_get_i64(va, i) };
        let eb = unsafe { gos_rt_vec_get_i64(vb, i) };
        if unsafe { gos_rt_enum_struct_eq(ea as *mut u8, eb as *mut u8, desc) } == 0 {
            return false;
        }
    }
    true
}

/// Element-wise structural equality of two `Vec<(String, self-enum)>` field
/// words (each a `*mut GosVec` of 16-byte `[cstr @ +0][enum ptr @ +8]` slots).
unsafe fn vec_str_self_enum_eq(a_word: i64, b_word: i64, desc: *const i64) -> bool {
    let va = a_word as *const GosVec;
    let vb = b_word as *const GosVec;
    if std::ptr::eq(va, vb) {
        return true;
    }
    if va.is_null() || vb.is_null() {
        return false;
    }
    let la = unsafe { (*va).len };
    if la != unsafe { (*vb).len } {
        return false;
    }
    for i in 0..la {
        let pa = unsafe { gos_rt_vec_get_ptr(va, i) };
        let pb = unsafe { gos_rt_vec_get_ptr(vb, i) };
        if pa.is_null() || pb.is_null() {
            if pa != pb {
                return false;
            }
            continue;
        }
        let ka = unsafe { pa.cast::<i64>().read_unaligned() };
        let kb = unsafe { pb.cast::<i64>().read_unaligned() };
        let sa: *const c_char = std::ptr::with_exposed_provenance(ka as usize);
        let sb: *const c_char = std::ptr::with_exposed_provenance(kb as usize);
        if !unsafe { gos_rt_str_eq(sa, sb) } {
            return false;
        }
        let ea = unsafe { pa.add(8).cast::<i64>().read_unaligned() };
        let eb = unsafe { pb.add(8).cast::<i64>().read_unaligned() };
        if unsafe { gos_rt_enum_struct_eq(ea as *mut u8, eb as *mut u8, desc) } == 0 {
            return false;
        }
    }
    true
}

/// Renders a `HashMap` to `{k: v, k2: v2}`, sorting entries by key so
/// the output is deterministic and byte-identical across tiers (an
/// `FxHashMap`'s bucket order is neither stable nor the same as the
/// VM's). Keys and values render the way the VM's `Display` does -
/// integers via `format_int`, strings bare. Empty maps and storage
/// shapes whose values aren't scalar (struct-keyed / byte-erased)
/// render as `{}`; the codegen only routes scalar-keyed, scalar- or
/// string-valued maps here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_format(m: *const GosMap) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if m.is_null() {
            return alloc_cstring(b"{}");
        }
        let map = unsafe { &*m };
        let storage = map.storage.lock();
        let mut out = String::from("{");
        let push_entry = |out: &mut String, first: &mut bool, k: &str, v: &str| {
            if *first {
                *first = false;
            } else {
                out.push_str(", ");
            }
            out.push_str(k);
            out.push_str(": ");
            out.push_str(v);
        };
        let mut first = true;
        match &*storage {
            MapStorage::I64I64(inner) => {
                let mut entries: Vec<(i64, i64)> = inner.iter().map(|(k, v)| (*k, *v)).collect();
                entries.sort_unstable_by_key(|(k, _)| *k);
                for (k, v) in entries {
                    push_entry(
                        &mut out,
                        &mut first,
                        &crate::builtins::format_int(k),
                        &crate::builtins::format_int(v),
                    );
                }
            }
            MapStorage::StrI64(inner) => {
                let mut entries: Vec<(&[u8], i64)> =
                    inner.iter().map(|(k, v)| (k.as_ref(), *v)).collect();
                entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
                for (k, v) in entries {
                    push_entry(
                        &mut out,
                        &mut first,
                        &String::from_utf8_lossy(k),
                        &crate::builtins::format_int(v),
                    );
                }
            }
            MapStorage::StrStr(inner) => {
                let mut entries: Vec<(&[u8], &[u8])> = inner
                    .iter()
                    .map(|(k, v)| (k.as_ref(), v.as_ref()))
                    .collect();
                entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
                for (k, v) in entries {
                    push_entry(
                        &mut out,
                        &mut first,
                        &String::from_utf8_lossy(k),
                        &String::from_utf8_lossy(v),
                    );
                }
            }
            MapStorage::I64Str(inner) => {
                let mut entries: Vec<(i64, &[u8])> =
                    inner.iter().map(|(k, v)| (*k, v.as_ref())).collect();
                entries.sort_unstable_by_key(|(k, _)| *k);
                for (k, v) in entries {
                    push_entry(
                        &mut out,
                        &mut first,
                        &crate::builtins::format_int(k),
                        &String::from_utf8_lossy(v),
                    );
                }
            }
            MapStorage::Empty | MapStorage::Bytes(_) | MapStorage::SkeyVal(_) => {}
        }
        out.push('}');
        alloc_cstring(out.as_bytes())
    })
}

/// Drops a `HashMap` allocated by [`gos_rt_map_new`] /
/// [`gos_rt_map_new_with_capacity`]. The MIR's drop-insertion pass
/// emits a call to this at every function return for any local
/// that owns a freshly-constructed map and isn't moved into the
/// return slot. Idempotent on null.
///
/// SAFETY: only call this on a pointer returned by one of the
/// runtime's `gos_rt_map_new*` constructors - the runtime's
/// [`GosMap`] layout includes a `parking_lot::Mutex<...>` and
/// dropping a binding-side `BindingGosMap` (two parallel `GosVec`
/// pointers) here would `Box::from_raw` the wrong shape and run
/// `Mutex::drop` over garbage. Use [`gos_rt_binding_map_free`] for
/// the binding-shaped aggregate instead.
/// Marks `m` as holding RC copy-blob values. Emitted by the MIR
/// lowering right after constructing a map whose declared value type is
/// a guarded aggregate. Null-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_set_blob_values(m: *mut GosMap) {
    if m.is_null() {
        return;
    }
    unsafe { &*m }
        .blob_values
        .store(true, std::sync::atomic::Ordering::Release);
}

fn map_has_blob_values(m: &GosMap) -> bool {
    m.blob_values.load(std::sync::atomic::Ordering::Acquire)
}

/// Release one stored blob value word (set-gated inside the RC layer
/// via the copy-blob provenance set membership of the pointer).
unsafe fn release_blob_value(word: i64) {
    if word != 0 {
        unsafe { crate::c_abi::rc::gos_rt_rc_release(word as usize as *mut u8) };
    }
}

/// Retain one stored blob value word before handing it out.
unsafe fn retain_blob_value(word: i64) {
    if word != 0 {
        unsafe { crate::c_abi::rc::gos_rt_rc_retain(word as usize as *mut u8) };
    }
}

/// Marks `m` shared across goroutines so every subsequent operation
/// takes the real lock instead of the goroutine-local fast path.
/// Codegen emits this at goroutine-spawn / channel-send escape points
/// for `HashMap`-typed values, on the owning goroutine *before* the map
/// is published - the same ordering `gos_rt_rc_mark_shared` relies on,
/// so the flip races with no concurrent reader. Aggregate (RC copy-blob)
/// values are marked shared too, since they become reachable from the
/// other goroutine through the now-shared map. Idempotent; null-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_mark_shared(m: *mut GosMap) {
    ffi_entry!((), {
        if m.is_null() {
            return;
        }
        let map = unsafe { &*m };
        if map_has_blob_values(map) {
            let storage = map.storage.lock();
            match &*storage {
                MapStorage::I64I64(inner) => {
                    for &v in inner.values() {
                        unsafe { crate::c_abi::rc::gos_rt_rc_mark_shared(v as usize as *mut u8) };
                    }
                }
                MapStorage::StrI64(inner) | MapStorage::SkeyVal(inner) => {
                    for &v in inner.values() {
                        unsafe { crate::c_abi::rc::gos_rt_rc_mark_shared(v as usize as *mut u8) };
                    }
                }
                _ => {}
            }
        }
        map.storage.mark_shared();
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_free(m: *mut GosMap) {
    ffi_entry!((), {
        if m.is_null() {
            return;
        }
        crate::c_abi::ledger::map_dec();
        let boxed = unsafe { Box::from_raw(m) };
        if map_has_blob_values(&boxed) {
            let storage = boxed.storage.lock();
            match &*storage {
                MapStorage::I64I64(inner) => {
                    for &v in inner.values() {
                        unsafe { release_blob_value(v) };
                    }
                }
                MapStorage::StrI64(inner) | MapStorage::SkeyVal(inner) => {
                    for &v in inner.values() {
                        unsafe { release_blob_value(v) };
                    }
                }
                _ => {}
            }
            drop(storage);
        }
        drop(boxed);
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
/// the `GosVec` header, the backing element buffer, and - when
/// `elem_kind != PRIMITIVE` - every pointer-bearing element
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
        // wholesale at `arena_pop` - never individually. Touching them here
        // via `Box::from_raw` / `Vec::from_raw_parts` would corrupt the
        // global allocator (the memory isn't its).
        if crate::c_abi::vec::vec_is_region(unsafe { &*v }) {
            return;
        }
        // RC: atomic decrement. Reclaim only when this thread held the last
        // reference (old count was 1). An aliased Vec still has live holders.
        // `Release` on the decrement publishes this thread's prior writes to
        // the element payloads; the `Acquire` fence on the final drop then
        // observes every other holder's writes before teardown - the standard
        // Arc drop discipline, without which a weakly-ordered target (aarch64,
        // a shipped tier) could reclaim a buffer while a peer's store is still
        // in flight.
        let old_rc = crate::c_abi::vec::vec_rc_atomic(unsafe { &*v })
            .fetch_sub(1, std::sync::atomic::Ordering::Release);
        if old_rc > 1 {
            return;
        }
        std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
        crate::c_abi::ledger::vec_dec();
        // Non-region headers are a single `Box<InlineVec>` (header +
        // inline element buffer); reconstruct it so the buffer that rides
        // with the header is reclaimed on `drop`, and only a separately
        // allocated (split) buffer needs its own `free_vec_buffer`.
        let inline_box = unsafe { Box::from_raw(v.cast::<crate::c_abi::vec::InlineVec>()) };
        let boxed = &inline_box.header;
        if !boxed.ptr.is_null() && boxed.cap > 0 {
            // Deep-free pointer-bearing element payloads BEFORE
            // reclaiming the backing buffer. Each branch walks the
            // first `len` slots - slots between `len` and `cap` were
            // never written and contain the zero-init produced by
            // `vec![0u8; bytes]` at construction time.
            // Guarded aggregate elements: release each element's
            // copy-blob children (set-gated in the walk) before the
            // buffer goes away.
            if boxed.elem_kind == vec_elem_kind::AGGR_GUARDED {
                unsafe { crate::c_abi::vec::vec_release_guarded_elements(boxed) };
            }
            // Owned-slot-children elements (materializer shims): free
            // each live embedded string / nested vec, including slots a
            // consumer loop never reached (the early-`break` path).
            if boxed.elem_kind == vec_elem_kind::AGGR_OWNED {
                unsafe { crate::c_abi::vec::vec_release_owned_children(boxed) };
            }
            if boxed.elem_kind != vec_elem_kind::PRIMITIVE && boxed.elem_bytes as usize == 8 {
                let count = boxed.len.max(0) as usize;
                // SAFETY: ptr is non-null + cap > 0 (checked above);
                // we only read `count <= len <= cap` slots of 8 bytes
                // each, all initialised by construction.
                let base = boxed.ptr;
                for i in 0..count {
                    // Slots hold child pointers exposed as integers by the
                    // flat-slot ABI in a byte buffer with no 8-byte
                    // alignment guarantee; read unaligned and recover
                    // provenance before the dereferencing free.
                    let raw = unsafe { base.add(i * 8).cast::<usize>().read_unaligned() };
                    if raw == 0 {
                        continue;
                    }
                    let slot: *mut u8 = std::ptr::with_exposed_provenance_mut(raw);
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
                        vec_elem_kind::RC_ENUM => {
                            // The vec owns each enum-node element (the push
                            // moved the frame's share in); release cascades
                            // through the node's own child meta.
                            unsafe { crate::c_abi::rc::gos_rt_rc_release(slot) };
                        }
                        _ => {}
                    }
                }
            }
            // Reclaim the element buffer only when it is a standalone
            // (split) allocation; an inline buffer is part of the header
            // block and goes away with `drop(inline_box)` below.
            if crate::c_abi::vec::vec_is_split(boxed) {
                let bytes = (boxed.cap as usize) * (boxed.elem_bytes as usize);
                // SAFETY: a split vec's buffer came from `alloc_vec_buffer(bytes)`.
                unsafe {
                    crate::c_abi::vec::free_vec_buffer(boxed.ptr.as_ptr(), bytes);
                }
            }
        }
        // Side-table entries are keyed by the header address the box
        // still occupies; removal must run AFTER the deep-free walks
        // above (which look the metas up by that address) and before
        // the header drops, so a reused address cannot inherit them.
        // Pass the `Box`'s own borrow, not the raw `v`, so the read of
        // `elem_kind` stays under the Box's exclusive ownership.
        crate::c_abi::vec::vec_elem_meta_remove(boxed);
        drop(inline_box);
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
/// underlying `FxHashMap`'s order - undefined-but-stable per
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
        // Sort by key for deterministic order that matches `values()`,
        // `iter()`, and the VM (a `FxHashMap`'s bucket order is neither
        // stable nor the same across tiers).
        let mut keys: Vec<i64> = match &*storage {
            MapStorage::I64I64(inner) => inner.keys().copied().collect(),
            MapStorage::I64Str(inner) => inner.keys().copied().collect(),
            _ => Vec::new(),
        };
        keys.sort_unstable();
        keys.iter().for_each(push_key);
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
        // Emit values in key-sorted order so `keys()` / `values()` /
        // `iter()` agree and the order is deterministic across tiers.
        let push_val = |v: i64| {
            let bytes = v.to_ne_bytes();
            unsafe { gos_rt_vec_push(out, bytes.as_ptr()) };
        };
        match &*storage {
            MapStorage::I64I64(inner) => {
                let mut entries: Vec<(i64, i64)> = inner.iter().map(|(k, v)| (*k, *v)).collect();
                entries.sort_unstable_by_key(|(k, _)| *k);
                for (_, v) in entries {
                    push_val(v);
                }
            }
            MapStorage::StrI64(inner) | MapStorage::SkeyVal(inner) => {
                let mut entries: Vec<(&[u8], i64)> =
                    inner.iter().map(|(k, v)| (k.as_ref(), *v)).collect();
                entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
                for (_, v) in entries {
                    push_val(v);
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
        // STRING-typed: the snapshot owns its key strings, so
        // `gos_rt_vec_free` reclaims them even on early `break`.
        let out = unsafe { crate::c_abi::vec::gos_rt_vec_new_typed(8, vec_elem_kind::STRING) };
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
        // Sort by key (lexicographic byte order, matching the VM's
        // `SmolStr` ordering) for deterministic, cross-tier order.
        let mut keys: Vec<&[u8]> = match &*storage {
            MapStorage::StrI64(inner) => inner.keys().map(|k| &**k).collect(),
            MapStorage::StrStr(inner) => inner.keys().map(|k| &**k).collect(),
            _ => Vec::new(),
        };
        keys.sort_unstable();
        for k in keys {
            push_key(k);
        }
        out
    })
}

/// Snapshots the string values of a string-valued `HashMap` into
/// a fresh `GosVec<*mut c_char>`. Mirrors `gos_rt_map_keys_str`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_values_str(m: *const GosMap) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        // STRING-typed - same ownership contract as `gos_rt_map_keys_str`.
        let out = unsafe { crate::c_abi::vec::gos_rt_vec_new_typed(8, vec_elem_kind::STRING) };
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
        // Values in key-sorted order so `keys()` / `values()` / `iter()`
        // agree and the order is deterministic across tiers.
        match &*storage {
            MapStorage::StrStr(inner) => {
                let mut entries: Vec<(&[u8], &[u8])> =
                    inner.iter().map(|(k, v)| (&**k, &**v)).collect();
                entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
                for (_, v) in entries {
                    push_val(v);
                }
            }
            MapStorage::I64Str(inner) => {
                let mut entries: Vec<(i64, &[u8])> =
                    inner.iter().map(|(k, v)| (*k, &**v)).collect();
                entries.sort_unstable_by_key(|(k, _)| *k);
                for (_, v) in entries {
                    push_val(v);
                }
            }
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
            // Struct/tuple-keyed maps store i64 values just like `I64I64`;
            // route them through the i64 snapshot so `m.values()` / `for v in
            // m.values()` see the real values instead of an empty Vec.
            MapStorage::SkeyVal(_) => {
                drop(storage);
                unsafe { gos_rt_map_values_i64(m) }
            }
            MapStorage::Empty => unsafe { gos_rt_vec_new(8) },
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
        let key_bytes = unsafe { crate::c_abi::string::gos_str_key_bytes(key) };
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

/// `m.pop(k) -> Option<V>` for a struct / tuple-keyed map. Content-hashes
/// the key via `build_skey`, removes the slot, and returns the previous
/// value in the `gos_rt_result_new` i128 layout (0 = Some, 1 = None),
/// matching [`gos_rt_map_pop_i64`]. The popped value's share transfers to
/// the caller, so no blob release fires here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_pop_skey(
    m: *mut GosMap,
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
        let map = unsafe { &mut *m };
        let mut storage = map.storage.lock();
        let popped: Option<i64> = match &mut *storage {
            MapStorage::SkeyVal(inner) => inner.remove(k.as_slice()),
            _ => None,
        };
        if popped.is_some() {
            map.len_cache = map.len_cache.saturating_sub(1);
        }
        match popped {
            Some(v) => unsafe { gos_rt_result_new(0, v) },
            None => none,
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
