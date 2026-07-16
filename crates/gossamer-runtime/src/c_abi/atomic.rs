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
    /// Goroutine that most recently published a release/SeqCst update.
    /// This is race-detector metadata only; the atomic value itself remains
    /// the language-visible storage.
    last_release_gid: AtomicI64,
}

fn record_atomic_acquire(a: &GosAtomicI64) {
    let from = a.last_release_gid.load(Ordering::Acquire);
    if from >= 0 {
        crate::race::record_sync(u32::try_from(from).unwrap_or(0), crate::race::current_gid());
    }
}

fn record_atomic_release(a: &GosAtomicI64) {
    a.last_release_gid
        .store(i64::from(crate::race::current_gid()), Ordering::Release);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_new(initial: i64) -> *mut GosAtomicI64 {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosAtomicI64 {
            inner: AtomicI64::new(initial),
            last_release_gid: AtomicI64::new(-1),
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
        let value = a.inner.load(Ordering::SeqCst);
        record_atomic_acquire(a);
        value
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
        record_atomic_release(a);
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_fetch_add(a: *mut GosAtomicI64, delta: i64) -> i64 {
    ffi_entry!(-1, {
        if a.is_null() {
            return 0;
        }
        let a = unsafe { &*a };
        let prior = a.inner.fetch_add(delta, Ordering::SeqCst);
        record_atomic_acquire(a);
        record_atomic_release(a);
        prior
    })
}

// ---------------------------------------------------------------
// Atomic<bool> primitive
// ---------------------------------------------------------------
//
// An `AtomicBool` shares the `GosAtomicI64` storage (0 / 1), so the
// constructor, load, and store live alongside the i64 family. The
// dedicated symbols let the compiled tiers pin the load result to
// `bool` so `{}` renders `true` / `false`, matching the VM, instead
// of the raw `i64` the shared `gos_rt_atomic_i64_load` returns.

/// Allocate a new atomic boolean initialised to `initial`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_bool_new(initial: bool) -> *mut GosAtomicI64 {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosAtomicI64 {
            inner: AtomicI64::new(i64::from(initial)),
            last_release_gid: AtomicI64::new(-1),
        }))
    })
}

/// Load an atomic boolean with sequentially-consistent ordering.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_bool_load(a: *const GosAtomicI64) -> bool {
    ffi_entry!(false, {
        if a.is_null() {
            return false;
        }
        let a = unsafe { &*a };
        let value = a.inner.load(Ordering::SeqCst) != 0;
        record_atomic_acquire(a);
        value
    })
}

/// Store into an atomic boolean with sequentially-consistent ordering.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_bool_store(a: *mut GosAtomicI64, val: bool) {
    ffi_entry!((), {
        if a.is_null() {
            return;
        }
        let a = unsafe { &*a };
        a.inner.store(i64::from(val), Ordering::SeqCst);
        record_atomic_release(a);
    });
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
        let value = a.inner.load(Ordering::Acquire);
        record_atomic_acquire(a);
        value
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
        record_atomic_release(a);
    });
}

/// Relaxed load - no synchronisation, only atomicity. Useful for
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
        let prior = a.inner.fetch_add(delta, Ordering::AcqRel);
        record_atomic_acquire(a);
        record_atomic_release(a);
        prior
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
            Ok(_) => {
                record_atomic_acquire(a);
                record_atomic_release(a);
                1
            }
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
            Ok(_) => {
                record_atomic_acquire(a);
                record_atomic_release(a);
                1
            }
            Err(_) => 0,
        }
    })
}

/// Atomic exchange - returns the previous value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_swap(a: *mut GosAtomicI64, val: i64) -> i64 {
    ffi_entry!(-1, {
        if a.is_null() {
            return 0;
        }
        let a = unsafe { &*a };
        let prior = a.inner.swap(val, Ordering::AcqRel);
        record_atomic_acquire(a);
        record_atomic_release(a);
        prior
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_store_records_the_publishing_goroutine() {
        crate::race::set_current_gid(701);
        let atomic = unsafe { gos_rt_atomic_i64_new(0) };
        unsafe { gos_rt_atomic_i64_store_release(atomic, 1) };
        let published = unsafe { &*atomic }.last_release_gid.load(Ordering::Acquire);
        assert_eq!(published, 701);
        unsafe { drop(Box::from_raw(atomic)) };
    }

    #[test]
    fn relaxed_store_does_not_create_a_happens_before_publication() {
        crate::race::set_current_gid(702);
        let atomic = unsafe { gos_rt_atomic_i64_new(0) };
        unsafe { gos_rt_atomic_i64_store_relaxed(atomic, 1) };
        let published = unsafe { &*atomic }.last_release_gid.load(Ordering::Acquire);
        assert_eq!(published, -1);
        unsafe { drop(Box::from_raw(atomic)) };
    }
}
