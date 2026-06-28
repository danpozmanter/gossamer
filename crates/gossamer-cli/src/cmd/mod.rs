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

/// Native stack reserved for the main thread that runs the bytecode VM.
///
/// Shared with the goroutine worker threads in `gossamer-interp` so
/// every VM-executing thread has the same generous reserve: the OS
/// main-thread stack is platform-dependent (8 MiB on Linux, roughly
/// 1 MiB on Windows), the VM's native dispatch and in-process JIT both
/// grow the real machine stack, and `MAX_CALL_DEPTH` only yields a
/// clean error if its frames fit before the OS guard page.
const VM_STACK_BYTES: usize = gossamer_interp::VM_THREAD_STACK_BYTES;

/// Runs `f` on a dedicated thread with a large native stack
/// ([`VM_STACK_BYTES`]) and returns its result, so the host's default
/// main-thread stack never bounds a Gossamer program's recursion
/// depth. A panic inside `f` is propagated to the caller unchanged.
/// Used by every VM-execution entry point (`gos run` / `test` /
/// `bench` / the REPL) and by the comptime fold, whose bytecode-VM
/// evaluation runs on the `build` / `check` main thread.
pub(crate) fn with_vm_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .name("gos-vm".to_string())
        .stack_size(VM_STACK_BYTES)
        .spawn(f)
        .expect("spawn VM execution thread")
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
}
