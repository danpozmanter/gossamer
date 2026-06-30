#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::must_use_candidate)]
#![allow(unused_unsafe)]
#![allow(clippy::wildcard_imports)]

use super::*;

// ---------------------------------------------------------------
// VecDeque<i64> - a ring-buffer FIFO queue for i64 values.
// The handle is a raw Box pointer; the caller is responsible for
// calling `gos_rt_deque_free` when done (or relying on GC reset).
// ---------------------------------------------------------------

/// Heap-allocated i64 ring-buffer deque.
pub struct GosDeque {
    inner: std::collections::VecDeque<i64>,
}

/// Create a new empty VecDeque, returning an opaque heap pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_deque_new() -> *mut GosDeque {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosDeque {
            inner: std::collections::VecDeque::new(),
        }))
    })
}

/// Append `value` to the back of the deque.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_deque_push_back(d: *mut GosDeque, value: i64) {
    ffi_entry!((), {
        if d.is_null() {
            return;
        }
        unsafe { &mut *d }.inner.push_back(value);
    });
}

/// Remove and return the front element as `Option<i64>` packed into i128.
/// Encoding: disc=0 (low i64) means `Some(value)`, disc=1 means `None`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_deque_pop_front(d: *mut GosDeque) -> i128 {
    ffi_entry!(0i128, {
        if d.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        match unsafe { &mut *d }.inner.pop_front() {
            Some(v) => unsafe { gos_rt_result_new(0, v) },
            None => unsafe { gos_rt_result_new(1, 0) },
        }
    })
}

/// Prepend `value` to the front of the deque.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_deque_push_front(d: *mut GosDeque, value: i64) {
    ffi_entry!((), {
        if d.is_null() {
            return;
        }
        unsafe { &mut *d }.inner.push_front(value);
    });
}

/// Remove and return the back element as `Option<i64>` packed into i128
/// (disc=0 `Some`, disc=1 `None`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_deque_pop_back(d: *mut GosDeque) -> i128 {
    ffi_entry!(0i128, {
        if d.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        match unsafe { &mut *d }.inner.pop_back() {
            Some(v) => unsafe { gos_rt_result_new(0, v) },
            None => unsafe { gos_rt_result_new(1, 0) },
        }
    })
}

/// Return the front element as `Option<i64>` without removing it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_deque_peek_front(d: *const GosDeque) -> i128 {
    ffi_entry!(0i128, {
        if d.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        match unsafe { &*d }.inner.front() {
            Some(v) => unsafe { gos_rt_result_new(0, *v) },
            None => unsafe { gos_rt_result_new(1, 0) },
        }
    })
}

/// Return the back element as `Option<i64>` without removing it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_deque_peek_back(d: *const GosDeque) -> i128 {
    ffi_entry!(0i128, {
        if d.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        match unsafe { &*d }.inner.back() {
            Some(v) => unsafe { gos_rt_result_new(0, *v) },
            None => unsafe { gos_rt_result_new(1, 0) },
        }
    })
}

/// Return the number of elements in the deque.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_deque_len(d: *const GosDeque) -> i64 {
    ffi_entry!(0, {
        if d.is_null() {
            return 0;
        }
        unsafe { &*d }.inner.len() as i64
    })
}

/// Return 1 if the deque is empty, 0 otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_deque_is_empty(d: *const GosDeque) -> i32 {
    ffi_entry!(1, {
        if d.is_null() {
            return 1;
        }
        i32::from(unsafe { &*d }.inner.is_empty())
    })
}

/// Release the deque heap allocation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_deque_free(d: *mut GosDeque) {
    ffi_entry!((), {
        if !d.is_null() {
            drop(unsafe { Box::from_raw(d) });
        }
    });
}
