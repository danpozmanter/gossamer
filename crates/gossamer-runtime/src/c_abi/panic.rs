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
    // Inline the active goroutine's call stack so the operator
    // can locate the failing frame without a separate SIGQUIT
    // round-trip. Empty when no frame info has been published
    // (e.g. a fall-back tier without stack-push hooks).
    let trace = crate::sigquit::render_active_panic_trace();
    if !trace.is_empty() {
        eprint!("{trace}");
    }
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

/// Pushes a call-stack frame on entry to a Gossamer function.
/// Codegen prologues emit one call per function entry; the
/// interpreter calls this directly. `function`, `file`, and `line`
/// identify the frame for panic dumps and SIGQUIT renders.
///
/// All three pointer arguments must be NUL-terminated C strings
/// or NULL. NULL is rendered as an empty string in the frame
/// record. The shim is reentrant-safe; the registry lock is held
/// only for the insertion.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_stack_push(
    function: *const c_char,
    file: *const c_char,
    line: u32,
) {
    let function = unsafe { cstr_to_string(function) };
    let file = unsafe { cstr_to_string(file) };
    crate::sigquit::stack_push(function, file, line);
}

/// Pops the topmost call-stack frame on return from a Gossamer
/// function. Tolerates over-pop (no-op when the stack is empty).
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_stack_pop() {
    crate::sigquit::stack_pop();
}

/// Updates the line number of the topmost call-stack frame.
/// Emitted by codegen at MIR-statement granularity so panic
/// dumps show the line of the most recent statement, not the
/// function entry. The frame's file path stays as it was set by
/// the matching `gos_rt_stack_push`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_stack_set_line(line: u32) {
    crate::sigquit::set_active_line(line);
}

unsafe fn cstr_to_string(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() }
    }
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
