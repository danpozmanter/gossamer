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

use std::sync::atomic::{AtomicI64, Ordering};

// ---------------------------------------------------------------
// Atomic<i64> primitive
// ---------------------------------------------------------------
//
// Heap-allocated `AtomicI64`. Used for shared work-counters
// (e.g. handing out chunk indices to workers) and for
// once-style flags. Mirrors Go's `atomic.Int64`.

pub struct GosAtomicI64 {
    inner: AtomicI64,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_new(initial: i64) -> *mut GosAtomicI64 {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosAtomicI64 {
            inner: AtomicI64::new(initial),
        }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_load(a: *const GosAtomicI64) -> i64 {
    ffi_entry!(-1, {
        if a.is_null() {
            return 0;
        }
        let a = unsafe { &*a };
        a.inner.load(Ordering::SeqCst)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_store(a: *mut GosAtomicI64, val: i64) {
    ffi_entry!((), {
        if a.is_null() {
            return;
        }
        let a = unsafe { &*a };
        a.inner.store(val, Ordering::SeqCst);
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_fetch_add(a: *mut GosAtomicI64, delta: i64) -> i64 {
    ffi_entry!(-1, {
        if a.is_null() {
            return 0;
        }
        let a = unsafe { &*a };
        a.inner.fetch_add(delta, Ordering::SeqCst)
    })
}

/// Acquire-ordered load. Cheaper than the SeqCst variant on
/// architectures with relaxed memory models (ARM64, RISC-V); on
/// x86 it lowers to the same instruction. Pair with the `_release`
/// store at the producer side for the standard release/acquire
/// pattern (`Mutex`-like handoff, lock-free queue head, etc.).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_load_acquire(a: *const GosAtomicI64) -> i64 {
    ffi_entry!(-1, {
        if a.is_null() {
            return 0;
        }
        let a = unsafe { &*a };
        a.inner.load(Ordering::Acquire)
    })
}

/// Release-ordered store, paired with `_load_acquire`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_store_release(a: *mut GosAtomicI64, val: i64) {
    ffi_entry!((), {
        if a.is_null() {
            return;
        }
        let a = unsafe { &*a };
        a.inner.store(val, Ordering::Release);
    });
}

/// Relaxed load — no synchronisation, only atomicity. Useful for
/// progress counters, generation tokens, and other observable-
/// from-anywhere values where ordering is enforced separately.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_load_relaxed(a: *const GosAtomicI64) -> i64 {
    ffi_entry!(-1, {
        if a.is_null() {
            return 0;
        }
        let a = unsafe { &*a };
        a.inner.load(Ordering::Relaxed)
    })
}

/// Relaxed store, paired with `_load_relaxed`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_store_relaxed(a: *mut GosAtomicI64, val: i64) {
    ffi_entry!((), {
        if a.is_null() {
            return;
        }
        let a = unsafe { &*a };
        a.inner.store(val, Ordering::Relaxed);
    });
}

/// AcqRel-ordered fetch_add. Use when both producer and consumer
/// observe the modification (CAS loops, ticket counters).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_fetch_add_acqrel(
    a: *mut GosAtomicI64,
    delta: i64,
) -> i64 {
    ffi_entry!(-1, {
        if a.is_null() {
            return 0;
        }
        let a = unsafe { &*a };
        a.inner.fetch_add(delta, Ordering::AcqRel)
    })
}

/// Compare-and-swap with SeqCst semantics. Returns `1` when the
/// swap happened, `0` when the observed value did not match
/// `expected`. Used to implement spin-locks and lock-free
/// data structures from compiled code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_cas(
    a: *mut GosAtomicI64,
    expected: i64,
    new: i64,
) -> i32 {
    ffi_entry!(-1, {
        if a.is_null() {
            return 0;
        }
        let a = unsafe { &*a };
        match a
            .inner
            .compare_exchange(expected, new, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => 1,
            Err(_) => 0,
        }
    })
}

/// Acquire-on-success / Acquire-on-failure CAS. Cheaper than the
/// SeqCst variant on relaxed-memory hosts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_cas_acq_rel(
    a: *mut GosAtomicI64,
    expected: i64,
    new: i64,
) -> i32 {
    ffi_entry!(-1, {
        if a.is_null() {
            return 0;
        }
        let a = unsafe { &*a };
        match a
            .inner
            .compare_exchange(expected, new, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => 1,
            Err(_) => 0,
        }
    })
}

/// Atomic exchange — returns the previous value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_swap(a: *mut GosAtomicI64, val: i64) -> i64 {
    ffi_entry!(-1, {
        if a.is_null() {
            return 0;
        }
        let a = unsafe { &*a };
        a.inner.swap(val, Ordering::AcqRel)
    })
}
