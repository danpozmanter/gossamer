//! Tree-walking interpreter for Gossamer.
//! Accepts an HIR program produced by [`gossamer_hir`] and executes
//! top-level functions directly, without first lowering to bytecode.
//! Used as the first executable end-to-end path through the frontend
//! and as a correctness oracle for the bytecode VM that arrives in
//!.
//! Values use reference-counted heap aggregates, mirroring the GC
//! semantics described in SPEC §3.3 even though the real garbage
//! collector does not land.

// Crate-level note: `unsafe` is forbidden in every module
// except `vm.rs`. The VM's inner dispatch loop uses
// `get_unchecked` for register / const-pool access; every
// index is bounded by the `FnChunk`'s compile-time counts.
// See `vm.rs` for the full invariant list.
#![deny(unsafe_code)]

mod builtins;
mod bytecode;
mod compile;
mod env;
mod flag_set_builtins;
mod http_client_builtins;
mod interp;
mod jit_call;
mod regex_builtins;
mod value;
mod vm;

pub use builtins::{
    TestTally, reset_test_tally, set_http_max_requests, set_program_args, set_stderr_writer,
    set_stdout_writer, set_struct_layouts, take_test_tally,
};
pub use jit_call::force_jit_disabled as set_jit_disabled;

/// Pushes `args` into the runtime's `ARGS_PTR` so JIT-compiled
/// `gos_rt_os_args` reads see the same list `os::args()` returns
/// in the bytecode VM. Process-lifetime ownership of the
/// `CString`s lives in a `Mutex` here; `*const c_char` doesn't
/// implement `Send` so we wrap the pointer table in a
/// `repr(transparent)` newtype that we explicitly mark `Send`.
///
/// Called by [`builtins::set_program_args`] which is the only
/// public entry point for both the bytecode and JIT arg lists.
#[doc(hidden)]
#[allow(clippy::similar_names)]
pub(crate) fn set_runtime_args(args: &[String]) {
    use std::ffi::CString;
    use std::os::raw::c_char;
    use std::sync::Mutex;

    /// `*const c_char` is not `Send`, so we wrap it. The values
    /// are read-only after `gos_rt_set_args` has copied them into
    /// its atomics; we never share the inner pointers across
    /// threads at the Rust type level.
    #[repr(transparent)]
    struct ArgPtr(*const c_char);
    // SAFETY: the pointers are owned by `CString`s held in the
    // same Mutex; nothing mutates them, and the runtime side
    // accesses them via SeqCst-ordered atomics.
    #[allow(unsafe_code)]
    unsafe impl Send for ArgPtr {}

    static OWNED: Mutex<(Vec<CString>, Vec<ArgPtr>)> = Mutex::new((Vec::new(), Vec::new()));

    let mut owned = OWNED.lock().expect("runtime-args mutex poisoned");
    let mut all = vec![CString::new("gos").expect("static label")];
    for a in args {
        let cstr = CString::new(a.as_bytes()).unwrap_or_else(|_| {
            let cleaned: Vec<u8> = a.bytes().filter(|b| *b != 0).collect();
            CString::new(cleaned).expect("cleaned bytes have no NUL")
        });
        all.push(cstr);
    }
    let ptrs: Vec<ArgPtr> = all.iter().map(|c| ArgPtr(c.as_ptr())).collect();
    let argc = i32::try_from(ptrs.len()).unwrap_or(1);
    let raw_argv = ptrs.as_ptr().cast::<*const c_char>();
    // SAFETY: `gos_rt_set_args` is `unsafe extern "C"` purely for
    // FFI uniformity; its preconditions are (argc >= 0) and
    // (argv addresses argc consecutive valid c-strings or is
    // NULL). Both hold here: `all` owns every string, `ptrs`
    // captures their `as_ptr()`, and the storage outlives the
    // runtime's read because `OWNED` is a `'static` Mutex.
    #[allow(unsafe_code)]
    unsafe {
        gossamer_runtime::c_abi::gos_rt_set_args(argc, raw_argv);
    }
    // Replace previous batch *after* the runtime has copied the
    // pointer values; dropping the prior CStrings now is safe.
    owned.0 = all;
    owned.1 = ptrs;
}

/// Flushes any data the JIT-compiled code has written to the
/// runtime's thread-local stdout buffer. The bytecode VM writes
/// through the Rust-side `set_stdout_writer` path which doesn't
/// touch this buffer, but JIT-compiled functions go through the
/// runtime's C-ABI `gos_rt_print_*` family which writes into
/// `STDOUT_BUF` and only flushes on `gos_rt_flush_stdout`.
///
/// The CLI calls this once after `vm.call("main", ...)` returns
/// so any JIT-promoted body's output reaches the user. Cheap
/// no-op when nothing was buffered.
pub fn flush_runtime_stdout() {
    // SAFETY: `gos_rt_flush_stdout` is `unsafe extern "C"` for
    // FFI uniformity but has no preconditions — it just drains
    // the per-thread `STDOUT_BUF` and writes to FD 1.
    #[allow(unsafe_code)]
    unsafe {
        gossamer_runtime::c_abi::gos_rt_flush_stdout();
    }
}
pub use bytecode::{FnChunk, Op};
pub use compile::compile_fn;
pub use env::Env;
pub use interp::{Interpreter, join_outstanding_goroutines};
pub use value::{Channel, Closure, RuntimeError, RuntimeResult, SmolStr, Value};
pub use vm::Vm;
