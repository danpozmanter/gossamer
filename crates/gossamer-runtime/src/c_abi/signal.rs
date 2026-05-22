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
use std::sync::atomic::Ordering;

use super::*;

// ---------------------------------------------------------------
// Signal notifier table — `os::signal::on` / `Notifier::wait`
// ---------------------------------------------------------------

struct SignalNotifier {
    flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    waiter: std::sync::Arc<SignalWaiter>,
}

#[derive(Default)]
struct SignalWaiter {
    mu: parking_lot::Mutex<()>,
    cv: parking_lot::Condvar,
}

struct SignalRegistry {
    notifiers: parking_lot::Mutex<Vec<Option<SignalNotifier>>>,
}

fn signal_registry() -> &'static SignalRegistry {
    static REGISTRY: std::sync::OnceLock<SignalRegistry> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| SignalRegistry {
        notifiers: parking_lot::Mutex::new(Vec::new()),
    })
}

// One relay thread per watched signal: blocks in signal-hook,
// then flips the flag and wakes the condvar for that notifier.
#[cfg(unix)]
fn install_signal_relay(
    sig_raw: i32,
    flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    waiter: std::sync::Arc<SignalWaiter>,
) {
    use signal_hook::iterator::Signals;
    let Ok(mut signals) = Signals::new([sig_raw]) else {
        return;
    };
    std::thread::Builder::new()
        .name(format!("gos-sig-{sig_raw}"))
        .spawn(move || {
            for _ in signals.forever() {
                flag.store(true, Ordering::Release);
                let _g = waiter.mu.lock();
                waiter.cv.notify_all();
            }
        })
        .ok();
}

/// `signal::on(sig_raw) -> i64` — registers a notifier for the
/// given raw signal number and returns an opaque handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_signal_on(sig_raw: i32) -> i64 {
    ffi_entry!(-1, {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let waiter = std::sync::Arc::new(SignalWaiter::default());
        #[cfg(unix)]
        install_signal_relay(
            sig_raw,
            std::sync::Arc::clone(&flag),
            std::sync::Arc::clone(&waiter),
        );
        // On non-unix platforms the signal number is unused.
        #[cfg(not(unix))]
        let _ = sig_raw;
        let notifier = SignalNotifier { flag, waiter };
        let mut notifiers = signal_registry().notifiers.lock();
        notifiers.push(Some(notifier));
        i64::try_from(notifiers.len() - 1).unwrap_or(-1)
    })
}

/// `signal::wait(handle)` — blocks until the registered signal fires.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_signal_wait(handle: i64) {
    ffi_entry!((), {
        let notifiers = signal_registry().notifiers.lock();
        let Some(Some(n)) = notifiers.get(handle as usize) else {
            return;
        };
        let flag = std::sync::Arc::clone(&n.flag);
        let waiter = std::sync::Arc::clone(&n.waiter);
        drop(notifiers);
        let mut g = waiter.mu.lock();
        loop {
            if flag.swap(false, Ordering::AcqRel) {
                return;
            }
            waiter.cv.wait(&mut g);
        }
    });
}

/// `signal::try_wait(handle) -> i32` — returns 1 if the signal
/// fired since the last check, 0 otherwise. Non-blocking.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_signal_try_wait(handle: i64) -> i32 {
    ffi_entry!(-1, {
        let notifiers = signal_registry().notifiers.lock();
        let Some(Some(n)) = notifiers.get(handle as usize) else {
            return 0;
        };
        let flag = std::sync::Arc::clone(&n.flag);
        drop(notifiers);
        i32::from(flag.swap(false, Ordering::AcqRel))
    })
}

/// Sorts a flat `[i64; len]` buffer in place using the closure
/// callback at `env`. The env's first word is the closure body
/// address; the body has signature `(env, i64, i64) -> i64`
/// (negative if a < b, positive if a > b, zero if equal),
/// matching `slice::sort_by`'s comparator contract.
///
/// Used by the MIR-side `xs.sort_by(closure)` lowering for fixed-
/// size arrays. The `Vec<T>` case routes through
/// `gos_rt_vec_sort_by_i64` instead. We pass the elements by
/// value (not pointer) because the typechecker today leaves the
/// closure params as plain `i64` rather than `&i64`, so the
/// closure body reads them as direct register values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_arr_sort_by_i64(p: *mut i64, len: i64, env: *const u8) {
    ffi_entry!((), {
        if p.is_null() || len <= 0 || env.is_null() {
            return;
        }
        let len_usize = len.max(0) as usize;
        let buf = unsafe { std::slice::from_raw_parts_mut(p, len_usize) };
        // Closure body sig: (env, i64, i64) -> i64.
        type CmpFn = unsafe extern "C" fn(env: *const u8, a: i64, b: i64) -> i64;
        // env[0] holds the body address (cranelift / LLVM both use
        // this layout for Fn(...)-shaped values).
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return;
        }
        super::fn_registry::verify(fn_addr_raw, super::fn_registry::FnKind::SortCmp);
        let cmp: CmpFn = unsafe { std::mem::transmute(fn_addr_raw) };
        buf.sort_by(|a, b| {
            let r = unsafe { cmp(env, *a, *b) };
            r.cmp(&0)
        });
    });
}

/// Sorts a `Vec<i64>` (heap `GosVec`) in place using the closure
/// callback at `env`. Mirrors [`gos_rt_arr_sort_by_i64`] for the
/// growable-vec receiver shape.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_sort_by_i64(v: *mut GosVec, env: *const u8) {
    ffi_entry!((), {
        if v.is_null() || env.is_null() {
            return;
        }
        let vec = unsafe { &mut *v };
        if vec.len <= 0 || vec.ptr.is_null() {
            return;
        }
        unsafe {
            gos_rt_arr_sort_by_i64(vec.ptr.cast::<i64>(), vec.len, env);
        }
    });
}

/// Sorts a `Vec<i64>` (heap `GosVec`) in ascending order in place.
/// Used by `xs.sort()` on integer vecs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_sort_i64(v: *mut GosVec) {
    ffi_entry!((), {
        if v.is_null() {
            return;
        }
        let vec = unsafe { &mut *v };
        if vec.len <= 0 || vec.ptr.is_null() {
            return;
        }
        let len_usize = vec.len.max(0) as usize;
        let buf = unsafe { std::slice::from_raw_parts_mut(vec.ptr.cast::<i64>(), len_usize) };
        buf.sort_unstable();
    });
}

/// Sorts a flat `[T; len]` buffer of `elem_bytes`-wide elements in
/// place using the closure callback at `env`. The closure body sig
/// is `(env, *const T, *const T) -> i64` — multi-slot aggregates
/// (Tuple / struct) are passed as pointers because the cranelift /
/// LLVM ABI already routes by-value aggregates that way. Used by
/// `xs.sort_by(closure)` for fixed-size arrays whose element type
/// is not single-slot scalar.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_arr_sort_by_aggr(
    p: *mut u8,
    len: i64,
    elem_bytes: i64,
    env: *const u8,
) {
    ffi_entry!((), {
        if p.is_null() || len <= 0 || elem_bytes <= 0 || env.is_null() {
            return;
        }
        let len_usize = len.max(0) as usize;
        let stride = elem_bytes.max(0) as usize;
        type CmpFn = unsafe extern "C" fn(env: *const u8, a: *const u8, b: *const u8) -> i64;
        let fn_addr_raw = unsafe { (env as *const usize).read() };
        if fn_addr_raw == 0 {
            return;
        }
        super::fn_registry::verify(fn_addr_raw, super::fn_registry::FnKind::SortCmpAggr);
        let cmp: CmpFn = unsafe { std::mem::transmute(fn_addr_raw) };
        // Indirect sort: rank the indices, then permute the buffer.
        // Sorting indices keeps the comparator pointer-stable across
        // swaps and avoids `unsafe` slice juggling for variable
        // strides that `slice::sort_by` doesn't support natively.
        let mut indices: Vec<usize> = (0..len_usize).collect();
        indices.sort_by(|&ai, &bi| {
            let pa = unsafe { p.add(ai * stride) };
            let pb = unsafe { p.add(bi * stride) };
            let r = unsafe { cmp(env, pa, pb) };
            r.cmp(&0)
        });
        // Permute via a temp buffer rather than in-place cycle
        // following — simpler, still O(n * stride) bytes and one
        // memcpy per element on the way back. Cycle-following would
        // halve peak memory but adds index bookkeeping that doesn't
        // earn its complexity at the sizes the comparator surface
        // sees in practice.
        let total = len_usize.checked_mul(stride).unwrap_or(0);
        let mut tmp: Vec<u8> = vec![0u8; total];
        for (new_idx, &old_idx) in indices.iter().enumerate() {
            unsafe {
                let src = p.add(old_idx * stride);
                let dst = tmp.as_mut_ptr().add(new_idx * stride);
                std::ptr::copy_nonoverlapping(src, dst, stride);
            }
        }
        unsafe {
            std::ptr::copy_nonoverlapping(tmp.as_ptr(), p, total);
        }
    });
}

/// Sorts a `Vec<T>` (heap `GosVec`) of multi-slot aggregate
/// elements in place. Stride comes from `vec.elem_bytes`, so the
/// MIR side doesn't have to thread it through separately.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_sort_by_aggr(v: *mut GosVec, env: *const u8) {
    ffi_entry!((), {
        if v.is_null() || env.is_null() {
            return;
        }
        let vec = unsafe { &mut *v };
        if vec.len <= 0 || vec.ptr.is_null() {
            return;
        }
        unsafe {
            gos_rt_arr_sort_by_aggr(vec.ptr.as_ptr(), vec.len, i64::from(vec.elem_bytes), env);
        }
    });
}

/// Handle table for the ABI 0.4 compiled-tier callback
/// dispatcher Each registration produces a `u64`
/// handle that compiled code can pass across an FFI boundary and
/// later invoke via [`gos_rt_callback_invoke`]. The table is
/// process-global; lookups acquire the mutex briefly to clone the
/// callback reference, then drop the lock before invocation so
/// the callback can register / unregister sibling handles
/// without deadlocking.
#[repr(C)]
struct CallbackEntry {
    /// Caller-supplied context pointer passed unchanged on every
    /// invocation. Typically a pointer to a heap-allocated
    /// closure environment owned by the binding crate.
    ctx: SyncRawPtr<u8>,
    /// C-ABI entry point — receives `(ctx, args, args_len,
    /// result_out)` and returns a status code (0 = ok, non-zero
    /// = caller-defined error).
    invoke: extern "C" fn(*const u8, *const u8, u32, *mut u8) -> i32,
}

static CALLBACK_TABLE: std::sync::OnceLock<
    parking_lot::Mutex<std::collections::HashMap<u64, CallbackEntry>>,
> = std::sync::OnceLock::new();
static NEXT_CALLBACK_HANDLE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn callback_table() -> &'static parking_lot::Mutex<std::collections::HashMap<u64, CallbackEntry>> {
    CALLBACK_TABLE.get_or_init(|| parking_lot::Mutex::new(std::collections::HashMap::new()))
}

/// Registers a callback in the process-global handle table.
/// Returns the assigned handle (non-zero on success; 0 reserved
/// for "no callback"). The caller is responsible for
/// [`gos_rt_callback_unregister`]ing when the closure's lifetime
/// ends — `BindingCallback`'s `Drop` impl handles this for
/// bindings that use the ABI 0.4 surface.
#[allow(unsafe_code, reason = "no_mangle FFI entry; raw fn pointer + ctx")]
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_callback_register(
    ctx: *const u8,
    invoke: extern "C" fn(*const u8, *const u8, u32, *mut u8) -> i32,
) -> u64 {
    ffi_entry!(0, {
        let handle = NEXT_CALLBACK_HANDLE.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        callback_table().lock().insert(
            handle,
            CallbackEntry {
                ctx: SyncRawPtr::new(ctx.cast_mut()),
                invoke,
            },
        );
        handle
    })
}

/// Removes a callback from the handle table. Idempotent on
/// unknown handles. After this call, [`gos_rt_callback_invoke`]
/// against the same handle returns `-1`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_callback_unregister(handle: u64) {
    ffi_entry!((), {
        if handle == 0 {
            return;
        }
        callback_table().lock().remove(&handle);
    });
}

/// Invokes the callback registered under `handle`. Returns the
/// status code from the callback (0 = ok, non-zero = error), or
/// `-1` when the handle is unknown.
///
/// The handle table mutex is released before the callback runs,
/// so the callback can register / unregister sibling handles
/// without deadlocking. `result_out` is zero-filled before
/// invocation so a callback that returns an error sentinel
/// (without touching the slot) leaves the caller observing zero
/// bytes instead of garbage.
#[allow(unsafe_code, reason = "no_mangle FFI entry; invokes raw fn pointer")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_callback_invoke(
    handle: u64,
    args: *const u8,
    args_len: u32,
    result_out: *mut u8,
) -> i32 {
    ffi_entry!(-1, {
        if handle == 0 {
            return -1;
        }
        // Best-effort zero of the first 16 bytes of result_out so an
        // error-path return doesn't leave stack garbage observable.
        if !result_out.is_null() {
            // SAFETY: caller declares result_out as a write-only
            // slot per the ABI. 16 bytes is the documented minimum.
            unsafe { std::ptr::write_bytes(result_out, 0, 16) };
        }
        // Clone the entry (ctx + fn ptr — both `Copy`) so we can drop
        // the lock before invocation. Without this drop, a callback
        // that recursively registers another handle would deadlock.
        let entry = {
            let table = callback_table().lock();
            match table.get(&handle) {
                Some(e) => CallbackEntry {
                    ctx: e.ctx,
                    invoke: e.invoke,
                },
                None => return -1,
            }
        };
        (entry.invoke)(entry.ctx.as_const_ptr(), args, args_len, result_out)
    })
}

/// A heap-allocated iterator over a `GosVec`. Created by
/// `gos_rt_arr_iter`; advanced one element at a time by
/// `gos_rt_arr_iter_next`.
#[repr(C)]
pub struct GosArrIter {
    /// Pointer to the vec being iterated. The caller must keep the
    /// vec alive for the iterator's lifetime.
    pub vec: SyncRawPtr<GosVec>,
    /// Next element index to yield.
    pub idx: i64,
}

/// Creates an iterator over `vec`, starting at index 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_arr_iter(vec: *mut GosVec) -> *mut GosArrIter {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosArrIter {
            vec: SyncRawPtr::new(vec),
            idx: 0,
        }))
    })
}

/// Advances the iterator by one and returns `GosResult { disc=0,
/// payload=element }` (Some) or `GosResult { disc=1, payload=0 }`
/// (None) when exhausted. Reads 8-byte-wide element slots only;
/// callers with other element widths must use a lower-level helper.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_arr_iter_next(iter: *mut GosArrIter) -> i128 {
    ffi_entry!(0i128, {
        if iter.is_null() {
            return gos_rt_result_new(1, 0);
        }
        let iter_ref = unsafe { &mut *iter };
        if iter_ref.vec.is_null() {
            return gos_rt_result_new(1, 0);
        }
        let vec_ref = unsafe { &*iter_ref.vec.as_ptr() };
        if iter_ref.idx >= vec_ref.len {
            return gos_rt_result_new(1, 0);
        }
        let value = unsafe { gos_rt_vec_get_i64(iter_ref.vec.as_ptr(), iter_ref.idx) };
        iter_ref.idx += 1;
        gos_rt_result_new(0, value)
    })
}

/// Frees a `GosArrIter` allocated by [`gos_rt_arr_iter`]. Does NOT
/// free the underlying vec — the vec is owned by the original local.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_arr_iter_free(iter: *mut GosArrIter) {
    ffi_entry!((), {
        if iter.is_null() {
            return;
        }
        drop(unsafe { Box::from_raw(iter) });
    });
}

/// Reads an `i64`-shaped element from a `Vec` (or any
/// 8-byte-elem `GosVec`) by index. Returns `0` when the receiver
/// is null or `idx` is out of range. Used by the MIR-side Vec
/// indexing path so `xs[0]` reads the data buffer instead of the
/// `GosVec` header's `len` field.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_get_i64(v: *const GosVec, idx: i64) -> i64 {
    ffi_entry!(-1, {
        if v.is_null() {
            return 0;
        }
        let vec = unsafe { &*v };
        if idx < 0 || idx >= vec.len {
            return 0;
        }
        let p = unsafe { vec.ptr.add((idx as usize) * (vec.elem_bytes as usize)) };
        unsafe { (p as *const i64).read_unaligned() }
    })
}

/// Writes an `i64`-shaped element to a `Vec` at `idx`. No-op for
/// null receivers or out-of-range indices (so a stale `xs[i] = v`
/// after a shrink doesn't trash unrelated memory).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_set_i64(v: *mut GosVec, idx: i64, value: i64) {
    ffi_entry!((), {
        if v.is_null() {
            return;
        }
        let vec = unsafe { &mut *v };
        if idx < 0 || idx >= vec.len {
            return;
        }
        let p = unsafe { vec.ptr.add((idx as usize) * (vec.elem_bytes as usize)) };
        unsafe { p.cast::<i64>().write_unaligned(value) };
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_get_ptr(v: *const GosVec, idx: i64) -> *mut u8 {
    ffi_entry!(std::ptr::null_mut(), {
        if v.is_null() {
            return std::ptr::null_mut();
        }
        let vec = unsafe { &*v };
        if idx < 0 || idx >= vec.len {
            return std::ptr::null_mut();
        }
        unsafe { vec.ptr.add((idx as usize) * (vec.elem_bytes as usize)) }
    })
}

/// Removes the last element of `v` and writes its bytes to
/// `out`. Returns 1 on success, 0 if the vec was empty. `out`
/// must be sized for `v.elem_bytes`.
/// `vec[lo..hi]` — copies the subrange `[lo, hi)` of `v`'s
/// elements into a fresh `GosVec` and returns a pointer to it.
/// Out-of-range bounds are clamped. Element bytes are copied
/// directly (the i64-erased ABI matches the rest of the Vec
/// surface).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_slice(v: *const GosVec, lo: i64, hi: i64) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        if v.is_null() {
            return unsafe { gos_rt_vec_new(8) };
        }
        let src = unsafe { &*v };
        let elem_bytes = src.elem_bytes;
        let len = src.len;
        let lo = lo.max(0).min(len);
        let hi = hi.max(lo).min(len);
        let count = hi - lo;
        let out = unsafe { gos_rt_vec_with_capacity(elem_bytes, count) };
        if !out.is_null() && count > 0 {
            for i in 0..count {
                unsafe {
                    let src_ptr = src.ptr.add(((lo + i) as usize) * (elem_bytes as usize));
                    gos_rt_vec_push(out, src_ptr);
                }
            }
        }
        out
    })
}

// --- 0.7.0 Vec method surface ---

/// `xs.first() -> Option<T>` packed as `*mut GosResult`. For
/// `Vec<i64>` / `Vec<*c_char>` / `Vec<f64>` (any 8-byte element)
/// the payload is the raw 8 bytes of element 0 cast to i64.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_first(v: *const GosVec) -> i128 {
    ffi_entry!(0i128, {
        if v.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let vec = unsafe { &*v };
        if vec.len <= 0 {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let p = vec.ptr.cast::<i64>();
        let value = unsafe { p.read_unaligned() };
        unsafe { gos_rt_result_new(0, value) }
    })
}

/// `xs.last() -> Option<T>` — sibling of `first`. Out-of-range on
/// an empty Vec returns None.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_last(v: *const GosVec) -> i128 {
    ffi_entry!(0i128, {
        if v.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let vec = unsafe { &*v };
        if vec.len <= 0 {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let off = ((vec.len - 1) as usize) * (vec.elem_bytes as usize);
        let p = unsafe { vec.ptr.add(off) };
        let value = unsafe { (p as *const i64).read_unaligned() };
        unsafe { gos_rt_result_new(0, value) }
    })
}

/// `xs.reversed() -> Vec<T>` — fresh Vec with the same elements in
/// reverse order. Element bytes are copied through the i64-erased
/// ABI matching the rest of the Vec surface.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_reversed(v: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        if v.is_null() {
            return unsafe { gos_rt_vec_new(8) };
        }
        let src = unsafe { &*v };
        let elem_bytes = src.elem_bytes;
        let len = src.len;
        let out = unsafe { gos_rt_vec_with_capacity(elem_bytes, len) };
        if !out.is_null() && len > 0 {
            for i in 0..len {
                let from_idx = len - 1 - i;
                let src_ptr = unsafe { src.ptr.add((from_idx as usize) * (elem_bytes as usize)) };
                unsafe { gos_rt_vec_push(out, src_ptr) };
            }
        }
        out
    })
}

/// `xs.index_of(&needle) -> Option<i64>` for an i64-shaped Vec.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_index_of_i64(v: *const GosVec, needle: i64) -> i128 {
    ffi_entry!(0i128, {
        if v.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let vec = unsafe { &*v };
        for i in 0..vec.len {
            let p = unsafe { vec.ptr.add((i as usize) * (vec.elem_bytes as usize)) };
            let elem = unsafe { (p as *const i64).read_unaligned() };
            if elem == needle {
                return unsafe { gos_rt_result_new(0, i) };
            }
        }
        unsafe { gos_rt_result_new(1, 0) }
    })
}

/// `xs.index_of(&needle) -> Option<i64>` for a Vec of c-string pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_index_of_str(v: *const GosVec, needle: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if v.is_null() || needle.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let vec = unsafe { &*v };
        for i in 0..vec.len {
            let p = unsafe { vec.ptr.add((i as usize) * (vec.elem_bytes as usize)) };
            let elem = unsafe { (p as *const *const c_char).read_unaligned() };
            if !elem.is_null() && unsafe { CStr::from_ptr(elem) == CStr::from_ptr(needle) } {
                return unsafe { gos_rt_result_new(0, i) };
            }
        }
        unsafe { gos_rt_result_new(1, 0) }
    })
}

/// `xs.count_of(&needle) -> i64` for an i64-shaped Vec.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_count_of_i64(v: *const GosVec, needle: i64) -> i64 {
    ffi_entry!(-1, {
        if v.is_null() {
            return 0;
        }
        let vec = unsafe { &*v };
        let mut count: i64 = 0;
        for i in 0..vec.len {
            let p = unsafe { vec.ptr.add((i as usize) * (vec.elem_bytes as usize)) };
            let elem = unsafe { (p as *const i64).read_unaligned() };
            if elem == needle {
                count += 1;
            }
        }
        count
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_count_of_str(v: *const GosVec, needle: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if v.is_null() || needle.is_null() {
            return 0;
        }
        let vec = unsafe { &*v };
        let mut count: i64 = 0;
        for i in 0..vec.len {
            let p = unsafe { vec.ptr.add((i as usize) * (vec.elem_bytes as usize)) };
            let elem = unsafe { (p as *const *const c_char).read_unaligned() };
            if !elem.is_null() && unsafe { CStr::from_ptr(elem) == CStr::from_ptr(needle) } {
                count += 1;
            }
        }
        count
    })
}

/// `xs.contains(&needle) -> bool` for an i64-shaped Vec.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_contains_i64(v: *const GosVec, needle: i64) -> i8 {
    ffi_entry!(0, {
        if v.is_null() {
            return 0;
        }
        let vec = unsafe { &*v };
        for i in 0..vec.len {
            let p = unsafe { vec.ptr.add((i as usize) * (vec.elem_bytes as usize)) };
            let elem = unsafe { (p as *const i64).read_unaligned() };
            if elem == needle {
                return 1;
            }
        }
        0
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_contains_str(v: *const GosVec, needle: *const c_char) -> i8 {
    ffi_entry!(0, {
        if v.is_null() || needle.is_null() {
            return 0;
        }
        let vec = unsafe { &*v };
        for i in 0..vec.len {
            let p = unsafe { vec.ptr.add((i as usize) * (vec.elem_bytes as usize)) };
            let elem = unsafe { (p as *const *const c_char).read_unaligned() };
            if !elem.is_null() && unsafe { CStr::from_ptr(elem) == CStr::from_ptr(needle) } {
                return 1;
            }
        }
        0
    })
}

/// `xs.slice(start, end) -> Result<Vec<T>, errors::Error>`.
/// Safe sub-range slicer. Inverted or out-of-range bounds return
/// `Err(errors::Error)`; valid bounds return `Ok(Vec<T>)` with the
/// elements byte-copied into a fresh Vec.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_slice_result(v: *const GosVec, start: i64, end: i64) -> i128 {
    ffi_entry!(0i128, {
        let len = if v.is_null() { 0 } else { unsafe { (*v).len } };
        if start < 0 || end < 0 || start > end || end > len {
            let msg = format!("slice: range [{start}, {end}) out of bounds for length {len}");
            let cs = std::ffi::CString::new(msg).unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let out = unsafe { gos_rt_vec_slice(v, start, end) };
        unsafe { gos_rt_result_new(0, out as i64) }
    })
}

/// `xs.slice(start, end) -> Result<Vec<i64>, errors::Error>` for
/// fixed-size i64 array receivers. The buffer is the raw inline
/// `[i64; N]` storage (no length prefix); MIR splices the
/// statically-known `len` from `TyKind::Array { len }` as the
/// second argument. Routed from MIR dispatch when the receiver
/// type is `[i64; N]` / `&[i64]`. Returns the same
/// `Result<Vec<i64>, errors::Error>` shape as
/// [`gos_rt_vec_slice_result`] so callers don't need a separate
/// match arm.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_intarr_slice_result(
    p: *const i64,
    len: i64,
    start: i64,
    end: i64,
) -> i128 {
    ffi_entry!(0i128, {
        if p.is_null() || start < 0 || end < 0 || start > end || end > len {
            let msg = format!("slice: range [{start}, {end}) out of bounds for length {len}");
            let cs = std::ffi::CString::new(msg).unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let count = end - start;
        let out = unsafe { gos_rt_vec_with_capacity(8, count) };
        if !out.is_null() && count > 0 {
            for i in 0..count {
                let element_index = (start + i) as usize;
                unsafe {
                    let src_ptr = p.add(element_index) as *const u8;
                    gos_rt_vec_push(out, src_ptr);
                }
            }
        }
        unsafe { gos_rt_result_new(0, out as i64) }
    })
}

/// `xs.slice(start, end) -> Result<Vec<f64>, errors::Error>` for
/// fixed-size f64 array receivers. Same layout contract as
/// [`gos_rt_intarr_slice_result`] — raw inline buffer plus a
/// statically-known length spliced by the MIR dispatcher.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_floatarr_slice_result(
    p: *const i64,
    len: i64,
    start: i64,
    end: i64,
) -> i128 {
    ffi_entry!(0i128, {
        if p.is_null() || start < 0 || end < 0 || start > end || end > len {
            let msg = format!("slice: range [{start}, {end}) out of bounds for length {len}");
            let cs = std::ffi::CString::new(msg).unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let count = end - start;
        let out = unsafe { gos_rt_vec_with_capacity(8, count) };
        if !out.is_null() && count > 0 {
            for i in 0..count {
                let element_index = (start + i) as usize;
                unsafe {
                    let src_ptr = p.add(element_index) as *const u8;
                    gos_rt_vec_push(out, src_ptr);
                }
            }
        }
        unsafe { gos_rt_result_new(0, out as i64) }
    })
}

/// `Vec::insert(xs, i, v) -> Result<Vec<T>, errors::Error>`. The
/// new Vec is returned wrapped in Ok; out-of-range indices return
/// Err. `value` is the raw 8-byte payload of the element being
/// inserted (i64 for `Vec<i64>`, `*const c_char` cast to i64 for
/// `Vec<String>` — matches the rest of the i64-erased Vec ABI).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_insert_safe(v: *const GosVec, idx: i64, value: i64) -> i128 {
    ffi_entry!(0i128, {
        let len = if v.is_null() { 0 } else { unsafe { (*v).len } };
        if idx < 0 || idx > len {
            let msg = format!("insert: index {idx} out of bounds for length {len}");
            let cs = std::ffi::CString::new(msg).unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let elem_bytes = if v.is_null() {
            8u32
        } else {
            unsafe { (*v).elem_bytes }
        };
        let out = unsafe { gos_rt_vec_with_capacity(elem_bytes, len + 1) };
        let val_ptr = std::ptr::addr_of!(value).cast::<u8>();
        // Push elements [0, idx) verbatim, then the inserted value,
        // then [idx, len). One straight-line build of the output Vec.
        if v.is_null() {
            unsafe { gos_rt_vec_push(out, val_ptr) };
        } else {
            let src = unsafe { &*v };
            for i in 0..idx {
                let src_ptr = unsafe { src.ptr.add((i as usize) * (elem_bytes as usize)) };
                unsafe { gos_rt_vec_push(out, src_ptr) };
            }
            unsafe { gos_rt_vec_push(out, val_ptr) };
            for i in idx..len {
                let src_ptr = unsafe { src.ptr.add((i as usize) * (elem_bytes as usize)) };
                unsafe { gos_rt_vec_push(out, src_ptr) };
            }
        }
        unsafe { gos_rt_result_new(0, out as i64) }
    })
}

/// `Vec::remove(xs, i) -> Result<T, errors::Error>` — returns the
/// removed element as Ok or Err on out-of-range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_remove_safe(v: *const GosVec, idx: i64) -> i128 {
    ffi_entry!(0i128, {
        let len = if v.is_null() { 0 } else { unsafe { (*v).len } };
        if v.is_null() || idx < 0 || idx >= len {
            let msg = format!("remove: index {idx} out of bounds for length {len}");
            let cs = std::ffi::CString::new(msg).unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let src = unsafe { &*v };
        let p = unsafe { src.ptr.add((idx as usize) * (src.elem_bytes as usize)) };
        let removed = unsafe { (p as *const i64).read_unaligned() };
        unsafe { gos_rt_result_new(0, removed) }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_pop(v: *mut GosVec, out: *mut u8) -> i32 {
    ffi_entry!(-1, {
        if v.is_null() || out.is_null() {
            return 0;
        }
        let vec = unsafe { &mut *v };
        if vec.len <= 0 {
            return 0;
        }
        vec.len -= 1;
        let src = unsafe { vec.ptr.add((vec.len as usize) * (vec.elem_bytes as usize)) };
        unsafe {
            std::ptr::copy_nonoverlapping(src, out, vec.elem_bytes as usize);
        }
        1
    })
}
