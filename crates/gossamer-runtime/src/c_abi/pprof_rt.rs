//! Runtime support for `std::pprof` on the compiled tiers.
//!
//! Each shim forwards to [`crate::pprof`], the same implementation the
//! bytecode VM's builtins call, so a profile taken under `gos run` and
//! one taken from a `gos build` binary render identically.

#![allow(clippy::missing_safety_doc)]

use std::os::raw::c_char;
use std::time::Duration;

/// Converts a rendered profile into an owned C string for the caller.
fn into_c_string(text: String) -> *mut c_char {
    std::ffi::CString::new(text).unwrap_or_default().into_raw()
}

/// `pprof::goroutine_profile() -> String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_pprof_goroutine_profile() -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        into_c_string(crate::pprof::goroutine_profile())
    })
}

/// `pprof::mutex_profile() -> String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_pprof_mutex_profile() -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        into_c_string(crate::pprof::mutex_profile())
    })
}

/// `pprof::block_profile() -> String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_pprof_block_profile() -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        into_c_string(crate::pprof::block_profile())
    })
}

/// `pprof::execution_trace(millis: i64) -> String`. A negative window
/// captures nothing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_pprof_execution_trace(millis: i64) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let window = Duration::from_millis(millis.max(0) as u64);
        into_c_string(crate::pprof::execution_trace(window))
    })
}

/// `pprof::route(path, query) -> Option<String>`, shaped as a
/// `*mut GosResult` with disc 0 = Some, 1 = None.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_pprof_route(path: *const c_char, query: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if path.is_null() {
            return unsafe { crate::c_abi::gos_rt_result_new(1, 0) };
        }
        let path = unsafe { crate::c_abi::gos_str_arg_string(path) };
        let query = if query.is_null() {
            String::new()
        } else {
            unsafe { crate::c_abi::gos_str_arg_string(query) }
        };
        match crate::pprof::route(&path, &query) {
            Some(body) => {
                let cs = crate::c_abi::alloc_cstring(body.as_bytes());
                unsafe { crate::c_abi::gos_rt_result_new(0, cs as i64) }
            }
            None => unsafe { crate::c_abi::gos_rt_result_new(1, 0) },
        }
    })
}
