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

use std::os::raw::c_char;

use super::*;

// ---------------------------------------------------------------
// Vec runtime — a `{ elem_bytes, len, cap, ptr }` struct
// ---------------------------------------------------------------

/// Element kind tag carried in the `GosVec` header so
/// `gos_rt_vec_free` can free element payloads instead of just the
/// backing byte buffer. Default `0` (primitive) preserves the
/// shallow-free behaviour every existing call site assumes; typed
/// vecs created via `gos_rt_vec_new_typed` opt in to deep free.
///
/// Encoding is deliberately small (one byte) so the field fits in
/// the existing 4-byte padding between `elem_bytes` (u32) and
/// `ptr` (8-byte aligned pointer). Adding it does not change the
/// struct size, the offset of `ptr`, or the offset of `len` — all
/// of which the codegen reads at fixed offsets.
pub mod vec_elem_kind {
    /// Element payload is a primitive value owning no other heap
    /// memory (i64, f64, u8, bool, etc.). Shallow free of the
    /// backing buffer is correct.
    pub const PRIMITIVE: u8 = 0;
    /// Element is a `*mut c_char` cstring; each element is freed
    /// via `gos_rt_str_free` before the buffer itself is reclaimed.
    pub const STRING: u8 = 1;
    /// Element is a `*mut GosVec`; each element is recursively
    /// freed via `gos_rt_vec_free`.
    pub const VEC: u8 = 2;
    /// Element is a `*mut GosMap`; each element is freed via
    /// `gos_rt_map_free`.
    pub const MAP: u8 = 3;
    /// Element is a `*mut GosError`; each element is freed via
    /// `gos_rt_error_free`.
    pub const ERROR: u8 = 4;
}

#[repr(C)]
pub struct GosVec {
    pub len: i64,
    pub cap: i64,
    pub elem_bytes: u32,
    /// Element-kind tag (see [`vec_elem_kind`]) so `gos_rt_vec_free`
    /// can deep-free pointer-bearing element types. Sits in the
    /// padding before `ptr` so the struct layout (size, ptr offset,
    /// len offset) is unchanged from prior 0.5 releases.
    pub elem_kind: u8,
    /// Padding bytes that preserve the historical 0.5 layout (`ptr`
    /// at the same offset as before the `elem_kind` tag was added).
    /// `pub` so sibling submodules can construct via the
    /// `GosVec { … }` literal; the `_` prefix marks it as inert
    /// filler that callers should ignore.
    #[allow(clippy::pub_underscore_fields)]
    pub _reserved: [u8; 3],
    pub ptr: SyncRawPtr<u8>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_new(elem_bytes: u32) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosVec {
            len: 0,
            cap: 0,
            elem_bytes,
            elem_kind: vec_elem_kind::PRIMITIVE,
            _reserved: [0; 3],
            ptr: SyncRawPtr::NULL,
        }))
    })
}

/// `gos_rt_vec_new`-like constructor that records the element kind
/// in the header so `gos_rt_vec_free` can deep-free pointer-bearing
/// payloads. `elem_kind` must be a value from [`vec_elem_kind`];
/// out-of-range values fall back to `PRIMITIVE` with an `eprintln!`
/// warning.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_new_typed(elem_bytes: u32, elem_kind: u8) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let kind = if elem_kind > vec_elem_kind::ERROR {
            eprintln!(
                "gos_rt_vec_new_typed: unknown elem_kind {elem_kind}; falling back to PRIMITIVE"
            );
            vec_elem_kind::PRIMITIVE
        } else {
            elem_kind
        };
        Box::into_raw(Box::new(GosVec {
            len: 0,
            cap: 0,
            elem_bytes,
            elem_kind: kind,
            _reserved: [0; 3],
            ptr: SyncRawPtr::NULL,
        }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_with_capacity(elem_bytes: u32, cap: i64) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        if cap <= 0 {
            return unsafe { gos_rt_vec_new(elem_bytes) };
        }
        let bytes = (cap as usize) * (elem_bytes as usize);
        // Zero-initialised so the backing storage is always valid to
        // read (clippy::uninit_vec). The interpreter never observes a
        // slot before it's been explicitly written via push/insert,
        // but zeroing is cheap and removes the UB risk.
        let mut buf: Vec<u8> = vec![0u8; bytes];
        let ptr = buf.as_mut_ptr();
        std::mem::forget(buf);
        Box::into_raw(Box::new(GosVec {
            len: 0,
            cap,
            elem_bytes,
            elem_kind: vec_elem_kind::PRIMITIVE,
            _reserved: [0; 3],
            ptr: SyncRawPtr::new(ptr),
        }))
    })
}

/// `gos_rt_vec_with_capacity` variant that records the element
/// kind in the header so `gos_rt_vec_free` can deep-free
/// pointer-bearing payloads. See [`vec_elem_kind`] for the tag
/// encoding. Out-of-range tags fall back to `PRIMITIVE` with an
/// `eprintln!` warning.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_with_capacity_typed(
    elem_bytes: u32,
    cap: i64,
    elem_kind: u8,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let kind = if elem_kind > vec_elem_kind::ERROR {
            eprintln!(
                "gos_rt_vec_with_capacity_typed: unknown elem_kind {elem_kind}; falling back to PRIMITIVE"
            );
            vec_elem_kind::PRIMITIVE
        } else {
            elem_kind
        };
        if cap <= 0 {
            return unsafe { gos_rt_vec_new_typed(elem_bytes, kind) };
        }
        let bytes = (cap as usize) * (elem_bytes as usize);
        let mut buf: Vec<u8> = vec![0u8; bytes];
        let ptr = buf.as_mut_ptr();
        std::mem::forget(buf);
        Box::into_raw(Box::new(GosVec {
            len: 0,
            cap,
            elem_bytes,
            elem_kind: kind,
            _reserved: [0; 3],
            ptr: SyncRawPtr::new(ptr),
        }))
    })
}

/// Builds a fresh `*mut GosVec` from a stack/heap array. Copies
/// `len * elem_bytes` bytes from `data` into a freshly-allocated
/// data buffer; `Box::into_raw`s the resulting GosVec header.
///
/// Used at the binding-call boundary to convert a Gossamer
/// `[T; N]` array literal (or similarly-shaped value) into the
/// `*mut GosVec` shape the binding's C-ABI thunk expects for a
/// `Vec<T>` parameter.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_from_arr(
    elem_bytes: u32,
    data: *const u8,
    len: i64,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let len = len.max(0);
        let n = (len as usize) * (elem_bytes as usize);
        let buf_ptr = if n == 0 || data.is_null() {
            std::ptr::null_mut()
        } else {
            let mut buf: Vec<u8> = vec![0u8; n];
            unsafe {
                std::ptr::copy_nonoverlapping(data, buf.as_mut_ptr(), n);
            }
            let p = buf.as_mut_ptr();
            std::mem::forget(buf);
            p
        };
        Box::into_raw(Box::new(GosVec {
            len,
            cap: len,
            elem_bytes,
            elem_kind: vec_elem_kind::PRIMITIVE,
            _reserved: [0; 3],
            ptr: SyncRawPtr::new(buf_ptr),
        }))
    })
}

/// Converts a flat 2-level nested array `[Array{T,inner_len}; outer_len]` into
/// a `Vec<*mut GosVec>` where every inner flat array has been promoted to a
/// heap-allocated `GosVec`. Needed when a `[[T]]` literal is coerced at a
/// call site that expects `Vec<Vec<T>>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_nested_arr_to_vec(
    inner_elem_bytes: i64,
    inner_len: i64,
    raw: *const u8,
    outer_len: i64,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        // Outer Vec holds pointer-sized elements (*mut GosVec).
        let outer = unsafe { gos_rt_vec_new(8) };
        if raw.is_null() || outer_len <= 0 || inner_len <= 0 || inner_elem_bytes <= 0 {
            return outer;
        }
        let stride = (inner_len as usize) * (inner_elem_bytes as usize);
        for i in 0..(outer_len as usize) {
            let inner_raw = unsafe { raw.add(i * stride) };
            let inner_vec =
                unsafe { gos_rt_vec_from_arr(inner_elem_bytes as u32, inner_raw, inner_len) };
            let inner_ptr_i64 = inner_vec as i64;
            let bytes = inner_ptr_i64.to_ne_bytes();
            unsafe { gos_rt_vec_push(outer, bytes.as_ptr()) };
        }
        outer
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_len(v: *const GosVec) -> i64 {
    ffi_entry!(-1, {
        if v.is_null() {
            return 0;
        }
        unsafe { (*v).len }
    })
}

/// Typed-i64 wrapper around [`gos_rt_vec_push`]. Spills the value
/// to a stack slot and forwards its address so the byte-erased
/// push helper can `memcpy` it into the vec's storage. Used by the
/// dynamic-count `[value; n]` lowering — passing an i64 directly
/// to the byte-erased helper would otherwise need a per-call-site
/// stack slot in cranelift.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_push_i64(v: *mut GosVec, value: i64) {
    ffi_entry!((), {
        let bytes = value.to_ne_bytes();
        unsafe { gos_rt_vec_push(v, bytes.as_ptr()) };
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_push(v: *mut GosVec, elem: *const u8) {
    ffi_entry!((), {
        if v.is_null() || elem.is_null() {
            return;
        }
        let vec = unsafe { &mut *v };
        if vec.len == vec.cap {
            // Grow geometrically (cap -> max(4, cap*2)).
            let new_cap = if vec.cap == 0 { 4 } else { vec.cap * 2 };
            let old_bytes = (vec.cap as usize) * (vec.elem_bytes as usize);
            let new_bytes = (new_cap as usize) * (vec.elem_bytes as usize);
            // Zero-initialised — see `gos_rt_vec_with_capacity`.
            let mut buf: Vec<u8> = vec![0u8; new_bytes];
            if !vec.ptr.is_null() && old_bytes > 0 {
                unsafe {
                    std::ptr::copy_nonoverlapping(vec.ptr.as_ptr(), buf.as_mut_ptr(), old_bytes);
                    // drop old allocation — sound only if `vec.ptr` was
                    // allocated through `Vec<u8>::Global`. Every helper
                    // that writes `vec.ptr` does so through that domain
                    // (see fix_architecture_ownership.md Stage 1.a).
                    Vec::from_raw_parts(vec.ptr.as_ptr(), old_bytes, old_bytes);
                }
            }
            vec.ptr = SyncRawPtr::new(buf.as_mut_ptr());
            vec.cap = new_cap;
            std::mem::forget(buf);
        }
        // 0.6.0: for STRING-typed vecs, copy the inbound string into a
        // tagged allocation so the deep-free path at vec_free time can
        // safely reclaim each element. Untagged strings (string
        // literals from .rodata, runtime-built CStrings) would
        // otherwise trip the STR_ALLOC_TAG check and leak silently.
        // For pointer-bearing kinds whose payload is already
        // heap-owned (VEC, MAP), transfer ownership unchanged.
        if vec.elem_kind == vec_elem_kind::STRING && vec.elem_bytes as usize == 8 {
            // SAFETY: elem points to an 8-byte slot holding a
            // *const c_char. STRING-typed vecs always carry 8-byte
            // pointer elements (enforced at vec_new_typed time).
            let src_cstr = unsafe { std::ptr::read_unaligned(elem.cast::<*const c_char>()) };
            let tagged = if src_cstr.is_null() {
                std::ptr::null_mut::<c_char>()
            } else {
                // SAFETY: src_cstr is null-terminated by ABI; copy the
                // bytes (without the NUL) into a fresh tagged
                // allocation. `from_ptr` walks until the NUL so this
                // works for both .rodata literals and heap strings.
                let bytes = unsafe { std::ffi::CStr::from_ptr(src_cstr).to_bytes() };
                alloc_cstring(bytes)
            };
            let dst = unsafe { vec.ptr.add((vec.len as usize) * (vec.elem_bytes as usize)) };
            unsafe {
                std::ptr::write_unaligned(dst.cast::<*mut c_char>(), tagged);
            }
            vec.len += 1;
            return;
        }
        let dst = unsafe { vec.ptr.add((vec.len as usize) * (vec.elem_bytes as usize)) };
        unsafe {
            std::ptr::copy_nonoverlapping(elem, dst, vec.elem_bytes as usize);
        }
        vec.len += 1;
    });
}

// ---------------------------------------------------------------
// Tagged-union encoding for `Result<T, E>` and `Option<T>`. The
// previous "happy-path" encoding stored just the payload value
// in the Result slot — meaning `Err(_)` and `None` had no
// distinguishing bit at runtime, so `match res { Ok(v) => …,
// Err(e) => … }` always took the Ok arm. A 2-slot heap struct
// (`disc`, `payload`) makes the Err / None case representable
// and lets pattern dispatch read the real discriminant.
//
// Convention: `disc == 0` = Ok / Some, `disc == 1` = Err / None.
// ---------------------------------------------------------------

#[repr(C)]
pub struct GosResult {
    pub disc: i64,
    pub payload: i64,
}

#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_new(disc: i64, payload: i64) -> *mut GosResult {
    ffi_entry!(std::ptr::null_mut(), {
        // `Box::into_raw` — single ownership domain across every
        // runtime helper. Previously the bump arena (`gos_rt_gc_alloc`)
        // backed this; the LLVM codegen's
        // `arena_save`/`arena_restore` could rewind the watermark
        // while a `*mut GosResult` was still live in caller code,
        // producing a dangling pointer that crashed at random sites
        // when the next allocator request reused the freed bytes.
        // See `~/dev/contexts/lang/fix_architecture_ownership.md`
        // Stage 4. Per-request leaks are reclaimed by the global GC
        // on a future cycle, not by arena reset.
        Box::into_raw(Box::new(GosResult { disc, payload }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_disc(p: *const GosResult) -> i64 {
    ffi_entry!(-1, {
        if p.is_null() {
            return 1;
        }
        unsafe { (*p).disc }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_dbg(p: i64) -> i64 {
    ffi_entry!(-1, {
        eprintln!("[rt] dbg called with raw i64 = {p:#x}");
        p
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_payload(p: *const GosResult) -> i64 {
    ffi_entry!(-1, {
        if p.is_null() {
            return 0;
        }
        unsafe { (*p).payload }
    })
}

/// `result.unwrap()` / `option.unwrap()`. Returns the wrapped
/// payload on the happy path; panics on Err / None.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_unwrap(p: *const GosResult) -> i64 {
    ffi_entry!(-1, {
        if p.is_null() {
            let cs = std::ffi::CString::new("called `Result::unwrap()` on an `Err` value").unwrap();
            unsafe { gos_rt_panic(cs.as_ptr()) };
            return 0;
        }
        let r = unsafe { &*p };
        if r.disc != 0 {
            let cs = std::ffi::CString::new("called `Result::unwrap()` on an `Err` value").unwrap();
            unsafe { gos_rt_panic(cs.as_ptr()) };
            return 0;
        }
        r.payload
    })
}

/// `result.unwrap_or(default)` / `option.unwrap_or(default)`.
/// Returns the payload on the happy path, else the supplied
/// default. Both inputs flow through as raw 64-bit slots so the
/// helper works for any inner type that fits in a single word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_unwrap_or(p: *const GosResult, default: i64) -> i64 {
    ffi_entry!(-1, {
        if p.is_null() {
            return default;
        }
        let r = unsafe { &*p };
        if r.disc == 0 { r.payload } else { default }
    })
}

/// `result.ok()` / `option.ok()`. Returns the payload on Ok/Some,
/// else 0. Mirrors the conventional "missing returns the zero
/// value of the wrapped type" semantics used elsewhere in the
/// compiled tier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_ok(p: *const GosResult) -> i64 {
    ffi_entry!(-1, {
        if p.is_null() {
            return 0;
        }
        let r = unsafe { &*p };
        if r.disc == 0 { r.payload } else { 0 }
    })
}

/// `result.err()`. Returns the error payload on Err, else 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_err(p: *const GosResult) -> i64 {
    ffi_entry!(-1, {
        if p.is_null() {
            return 0;
        }
        let r = unsafe { &*p };
        if r.disc == 1 { r.payload } else { 0 }
    })
}

/// `result.ok_or(new_err)`. On Ok, returns the receiver unchanged;
/// on Err, returns a new Result with `new_err` as the payload.
/// Lets `parse().ok_or("not a number".to_string())?` replace the
/// raw ParseError with a domain-meaningful message.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_ok_or(p: *mut GosResult, new_err: i64) -> *mut GosResult {
    ffi_entry!(std::ptr::null_mut(), {
        if p.is_null() {
            return unsafe { gos_rt_result_new(1, new_err) };
        }
        let r = unsafe { &*p };
        if r.disc == 0 {
            p
        } else {
            unsafe { gos_rt_result_new(1, new_err) }
        }
    })
}

/// `result.is_ok()` / `option.is_some()`. Returns 1 on Ok/Some,
/// 0 on Err/None or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_is_ok(p: *const GosResult) -> i64 {
    ffi_entry!(-1, {
        if p.is_null() {
            return 0;
        }
        let r = unsafe { &*p };
        i64::from(r.disc == 0)
    })
}

/// `result.is_err()` / `option.is_none()`. Returns 1 on Err/None
/// or null, 0 on Ok/Some.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_is_err(p: *const GosResult) -> i64 {
    ffi_entry!(-1, {
        if p.is_null() {
            return 1;
        }
        let r = unsafe { &*p };
        i64::from(r.disc != 0)
    })
}

/// Maps a `gos_main` return value to a process exit code.
/// Treats a heap-shaped pointer as a `*mut GosResult` and reads
/// its `disc`; falls back to the raw value (truncated) for
/// non-pointer returns. Also blocks until every outstanding
/// goroutine has settled so their stdout reaches the user
/// before the process exits.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_main_exit_code(raw: i64) -> i32 {
    ffi_entry!(-1, {
        // Wait for every goroutine spawned via `go expr` to finish.
        // Without this the M:N pool keeps workers alive on a Condvar
        // while the main thread races straight to `_exit`, dropping
        // unflushed stdout and any worker output that hadn't yet
        // reached the underlying file descriptor.
        // Wait for outstanding goroutines so their stdout reaches
        // the user before the process exits. The M:N pool's worker
        // threads boot lazily on first `spawn`, so a fast main
        // (`go expr; return`) can race the worker start-up. Two
        // guards: (1) seed wait so the worker pool has time to
        // dequeue the first task, and (2) settle wait so a
        // task that just decremented `live` has time to actually
        // emit its stdout before the next sample.
        let sched = crate::sched_global::scheduler();
        let start = std::time::Instant::now();
        let deadline = start + std::time::Duration::from_secs(5);
        let mut consecutive_settled = 0_u32;
        let mut iters = 0_u64;
        while std::time::Instant::now() < deadline {
            let live = sched.live_goroutines();
            let stats = sched.stats();
            let settled =
                live == 0 && stats.spawned == stats.finished && start.elapsed().as_millis() >= 100;
            if settled {
                consecutive_settled += 1;
                if consecutive_settled >= 5 {
                    break;
                }
            } else {
                consecutive_settled = 0;
            }
            iters += 1;
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = iters;
        // Flush any buffered stdout that workers wrote so it
        // reaches the user before the process exits.
        unsafe { gos_rt_flush_stdout() };
        if raw == 0 {
            return 0;
        }
        let p = raw as usize;
        let looks_like_heap = p > 0x10000 && p.trailing_zeros() >= 3;
        if !looks_like_heap {
            return raw as i32;
        }
        let disc = unsafe { (*(raw as *const GosResult)).disc };
        disc as i32
    })
}
