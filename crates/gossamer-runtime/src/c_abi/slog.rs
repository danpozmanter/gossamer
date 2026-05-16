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

// ---------------------------------------------------------------
// slog — simple stderr logger.
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_slog_info(msg: *const c_char) {
    ffi_entry!((), {
        if msg.is_null() {
            return;
        }
        let m = unsafe { CStr::from_ptr(msg).to_string_lossy() };
        eprintln!("INFO: {m}");
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_slog_warn(msg: *const c_char) {
    ffi_entry!((), {
        if msg.is_null() {
            return;
        }
        let m = unsafe { CStr::from_ptr(msg).to_string_lossy() };
        eprintln!("WARN: {m}");
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_slog_error(msg: *const c_char) {
    ffi_entry!((), {
        if msg.is_null() {
            return;
        }
        let m = unsafe { CStr::from_ptr(msg).to_string_lossy() };
        eprintln!("ERROR: {m}");
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_slog_debug(msg: *const c_char) {
    ffi_entry!((), {
        if msg.is_null() {
            return;
        }
        let m = unsafe { CStr::from_ptr(msg).to_string_lossy() };
        eprintln!("DEBUG: {m}");
    });
}
