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
// testing module - minimal `check`, `check_eq`, `check_ok` that
// log to stderr. Real test discovery / reporting is done via the
// interpreter today; these stubs make the example compile.
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_testing_check(cond: bool, msg: *const c_char) -> bool {
    ffi_entry!(false, {
        if !cond {
            let m = if msg.is_null() {
                "check failed".to_string()
            } else {
                unsafe { CStr::from_ptr(msg).to_string_lossy().into_owned() }
            };
            eprintln!("test check failed: {m}");
        }
        cond
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_testing_check_eq_i64(a: i64, b: i64, msg: *const c_char) -> bool {
    ffi_entry!(false, {
        let ok = a == b;
        if !ok {
            let m = if msg.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(msg).to_string_lossy().into_owned() }
            };
            eprintln!("test check_eq failed: {a} != {b} ({m})");
        }
        ok
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_testing_wait_for_scheduler_idle(timeout_ms: i64) -> bool {
    ffi_entry!(false, {
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms.max(0) as u64);
        let scheduler = crate::sched_global::scheduler();
        loop {
            let stats = scheduler.stats();
            if scheduler.live_goroutines() == 0 && stats.spawned == stats.finished {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    })
}
