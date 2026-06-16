//! Subcommand implementations for `gos`.
//!
//! `main.rs` parses the `crate::cli::Cli` enum and dispatches each
//! variant into the matching module here. Keeping the per-command
//! logic in dedicated files makes `main.rs` a routing table - the
//! place to look when a flag stops landing where you expect.

pub(crate) mod attr_walk;
pub(crate) mod bench;
pub(crate) mod bindgen;
pub(crate) mod build;
pub(crate) mod check;
pub(crate) mod clean;
pub(crate) mod env_cmd;
pub(crate) mod explain;
pub mod feature_status;
pub(crate) mod fmt_cmd;
pub(crate) mod lint_cmd;
pub(crate) mod lsp_cmd;
pub(crate) mod parse;
pub(crate) mod pkg;
pub(crate) mod run;
pub(crate) mod scaffold;
pub(crate) mod skill_prompt;
pub(crate) mod test;
pub(crate) mod traceback;
pub(crate) mod watch;

pub(crate) use run::RunMode;
pub(crate) use test::TestOpts;

/// Native stack reserved for the thread that runs the bytecode VM.
///
/// The OS main-thread stack is platform-dependent (8 MiB on Linux,
/// roughly 1 MiB on Windows). The VM's native dispatch and the
/// in-process JIT compile pass both grow the real machine stack, so a
/// deep-recursion or `arena` program that runs on Linux overflows the
/// smaller Windows main stack and aborts with `STATUS_STACK_OVERFLOW`.
/// `MAX_CALL_DEPTH` caps Gossamer-level recursion, but its frames must
/// fit on the native stack for the cap to be reached before the OS
/// guard page is hit. A fixed, generous reserve makes execution
/// uniform across hosts; the pages are virtual until touched.
const VM_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Runs `f` on a dedicated thread with a large native stack
/// ([`VM_STACK_BYTES`]) and returns its result, so the host's default
/// main-thread stack never bounds a Gossamer program's recursion
/// depth. A panic inside `f` is propagated to the caller unchanged.
/// Used by every VM-execution entry point (`gos run` / `test` /
/// `bench` / the REPL).
pub(crate) fn with_vm_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .name("gos-vm".to_string())
        .stack_size(VM_STACK_BYTES)
        .spawn(f)
        .expect("spawn VM execution thread")
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
}
