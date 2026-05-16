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

use super::*;

// ---------------------------------------------------------------
// Panic
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_panic(msg: *const c_char) {
    let text = if msg.is_null() {
        "panic".to_string()
    } else {
        unsafe { CStr::from_ptr(msg).to_string_lossy().into_owned() }
    };
    // Match the unified diagnostic-code prefix the VM /
    // tree-walker use so both execution modes tag panics with
    // `error[GX0005]` — keeps user-visible stderr identical
    // whether `gos run` took the native path or fell back.
    eprintln!("error[GX0005]: panic: {text}");
    // per-goroutine panic isolation. If the
    // panic originates inside a spawned goroutine, raise a Rust
    // panic the coroutine wrapper catches — the scheduler
    // continues running other goroutines. If we're on the main
    // thread (no active coroutine), keep the pre-0.6 behaviour
    // and abort the process: a panic in `fn main()` is fatal,
    // just like in Rust.
    if gossamer_coro::in_goroutine() {
        // Set the panicked flag explicitly so the test-helper
        // path observes it even if catch_unwind has already
        // converted the panic into a typed Err.
        std::panic::panic_any(text);
    }
    std::process::abort();
}

/// Returns 1 if any spawned goroutine has panicked since process
/// start, 0 otherwise. Sticky once set. Test helpers and
/// long-running services call this to assert clean execution
/// after a wait-group join.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_goroutine_panicked() -> i32 {
    i32::from(gossamer_coro::any_goroutine_panicked())
}

/// Panic helper for the dynamic array-index bounds check emitted
/// by the Cranelift and LLVM back-ends. Prints a diagnostic naming
/// the operation, the offending index, and the array length, then
/// routes through `gos_rt_panic` so the unified `error[GX0005]`
/// prefix and the panic-on-abort semantics stay consistent.
///
/// `what` is a static C string (e.g. `"array index"`) identifying
/// the failing access. NULL is tolerated and rendered as
/// `"array index"`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_panic_oob(what: *const c_char, idx: i64, len: i64) -> ! {
    let label = if what.is_null() {
        "array index".to_string()
    } else {
        unsafe { CStr::from_ptr(what).to_string_lossy().into_owned() }
    };
    let msg = format!("{label} out of bounds: the len is {len} but the index is {idx}");
    let cmsg = std::ffi::CString::new(msg).unwrap_or_else(|_| {
        std::ffi::CString::new("array index out of bounds").unwrap_or_default()
    });
    unsafe { gos_rt_panic(cmsg.as_ptr()) };
    // `gos_rt_panic` calls `std::process::abort`, so this is
    // unreachable. The explicit `abort` keeps the `-> !` return
    // type honest if `gos_rt_panic` is ever changed to unwind.
    std::process::abort();
}

// ---------------------------------------------------------------
// Exit
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_exit(code: i32) -> ! {
    // signal the netpoller thread to drain its
    // current `poll()` cycle before `std::process::exit` kills it.
    // Without this, in-flight TCP send buffers were terminated by
    // RST (process death) instead of FIN (graceful close). The
    // poller checks the flag at the top of each tick (1 ms ceiling).
    crate::sched_global::request_shutdown();
    // Drain the runtime's line-buffered stdout cache before
    // process exit. Without the flush, `println!("...")` followed
    // by `os::exit(N)` produces no output — `std::process::exit`
    // skips the C++/atexit handlers that would normally drain
    // stdio.
    unsafe {
        gos_rt_flush_stdout();
    }
    std::process::exit(code);
}

/// Returns the current process ID. Wraps `std::process::id`. The
/// LLVM and cranelift backends call this for `process::id()` in
/// Gossamer source.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_process_id() -> u32 {
    ffi_entry!(0, { std::process::id() })
}

/// Aborts the current process without unwinding. Wraps
/// `std::process::abort`. Used by `process::abort()` in Gossamer
/// source. Doesn't flush stdout — abort semantics.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_process_abort() -> ! {
    std::process::abort();
}
