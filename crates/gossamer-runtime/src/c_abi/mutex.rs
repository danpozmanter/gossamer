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
// Mutex<T> primitive
// ---------------------------------------------------------------
//
// Naked synchronisation primitive — no payload, no RAII guard,
// the user follows lock/unlock discipline. Backed by
// `parking_lot::Mutex<()>` so contention uses futexes on
// Linux. The pointer is heap-allocated and shared by every
// goroutine that captures it.

pub struct GosMutex {
    inner: parking_lot::Mutex<()>,
    /// Goroutine id of the most recent unlocker. Read by the next
    /// lock acquirer to record a happens-before edge into the race
    /// detector. `-1` means "never been locked".
    last_unlocker: AtomicI64,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mutex_new() -> *mut GosMutex {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosMutex {
            inner: parking_lot::Mutex::new(()),
            last_unlocker: AtomicI64::new(-1),
        }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mutex_lock(m: *mut GosMutex) {
    ffi_entry!((), {
        if m.is_null() {
            return;
        }
        let m = unsafe { &*m };
        // Forget the guard — the user calls unlock explicitly.
        let guard = m.inner.lock();
        std::mem::forget(guard);
        let from = m.last_unlocker.load(Ordering::Acquire);
        if from >= 0 {
            crate::race::record_sync(u32::try_from(from).unwrap_or(0), crate::race::current_gid());
        }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mutex_unlock(m: *mut GosMutex) {
    ffi_entry!((), {
        if m.is_null() {
            return;
        }
        // SAFETY: matched with the `forget` in lock — the lock is
        // held and we now release it. Releasing an unlocked mutex
        // is undefined; the user's discipline (one lock per
        // unlock) is required.
        let m = unsafe { &*m };
        m.last_unlocker
            .store(i64::from(crate::race::current_gid()), Ordering::Release);
        unsafe { m.inner.force_unlock() };
    });
}
