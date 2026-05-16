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
// WaitGroup primitive
// ---------------------------------------------------------------
//
// Mirrors `sync.WaitGroup` in Go. `add(n)` bumps a counter,
// `done()` decrements, `wait()` blocks until the counter hits
// zero. Implemented as `(parking_lot::Mutex<i64>, parking_lot
// ::Condvar)` plus a sticky error flag so misuse never panics
// while the lock is held.

pub struct GosWaitGroup {
    counter: parking_lot::Mutex<i64>,
    cv: parking_lot::Condvar,
    /// Sticky misuse marker. Bit 0 set on underflow (done called
    /// more than add granted), bit 1 set on overflow (counter would
    /// pass `i64::MAX`). Surfaced via `gos_rt_wg_error` so callers
    /// can fail loudly without taking a panic path while the
    /// counter mutex is held.
    error: AtomicI64,
    /// Goroutine id of the most recent caller of `done`. Used by
    /// `wait` to record a happens-before edge so the race detector
    /// observes that the waiter sees everything the done-callers
    /// did before signalling.
    last_done: AtomicI64,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_wg_new() -> *mut GosWaitGroup {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosWaitGroup {
            counter: parking_lot::Mutex::new(0),
            cv: parking_lot::Condvar::new(),
            error: AtomicI64::new(0),
            last_done: AtomicI64::new(-1),
        }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_wg_add(wg: *mut GosWaitGroup, n: i64) -> i64 {
    ffi_entry!(-1, {
        if wg.is_null() {
            return -1;
        }
        let wg = unsafe { &*wg };
        let mut c = wg.counter.lock();
        if let Some(v) = c.checked_add(n) {
            *c = v;
            if v < 0 {
                wg.error.fetch_or(1, Ordering::Relaxed);
            }
            if v <= 0 {
                wg.cv.notify_all();
            }
            v
        } else {
            wg.error.fetch_or(2, Ordering::Relaxed);
            -1
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_wg_done(wg: *mut GosWaitGroup) -> i64 {
    ffi_entry!(-1, {
        if wg.is_null() {
            return -1;
        }
        let wg = unsafe { &*wg };
        let mut c = wg.counter.lock();
        *c -= 1;
        let value = *c;
        if value < 0 {
            wg.error.fetch_or(1, Ordering::Relaxed);
        }
        if value <= 0 {
            wg.cv.notify_all();
        }
        drop(c);
        wg.last_done
            .store(i64::from(crate::race::current_gid()), Ordering::Release);
        value
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_wg_wait(wg: *mut GosWaitGroup) {
    ffi_entry!((), {
        if wg.is_null() {
            return;
        }
        let wg = unsafe { &*wg };
        let mut c = wg.counter.lock();
        while *c > 0 {
            wg.cv.wait(&mut c);
        }
        drop(c);
        let from = wg.last_done.load(Ordering::Acquire);
        if from >= 0 {
            crate::race::record_sync(u32::try_from(from).unwrap_or(0), crate::race::current_gid());
        }
    });
}

/// Returns the sticky misuse bitmask: 0 = ok, 1 = underflow seen,
/// 2 = overflow seen, 3 = both. Reading does not clear the flag;
/// `gos_rt_wg_error_clear` resets it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_wg_error(wg: *const GosWaitGroup) -> i64 {
    ffi_entry!(-1, {
        if wg.is_null() {
            return 0;
        }
        let wg = unsafe { &*wg };
        wg.error.load(Ordering::Relaxed)
    })
}

/// Clears the sticky misuse bitmask. Returns the value observed
/// before the clear so callers can act on whatever was queued.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_wg_error_clear(wg: *mut GosWaitGroup) -> i64 {
    ffi_entry!(-1, {
        if wg.is_null() {
            return 0;
        }
        let wg = unsafe { &*wg };
        wg.error.swap(0, Ordering::Relaxed)
    })
}
