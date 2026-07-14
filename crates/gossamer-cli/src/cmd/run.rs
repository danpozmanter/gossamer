//! `gos run [PATH]` - execute a program through the bytecode VM.
//!
//! The register-based bytecode VM is the only execution engine for
//! `gos run`. The tree-walker interpreter was removed in 0.14.0 (the
//! `--tree-walker` flag in 0.5.0); no CLI / env-var path selects an
//! engine.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::loaders::{load_and_check, profile_rss_stage};
use crate::paths::{read_entry_source, resolve_entry_arg};

/// How `gos run` executes a program. Single-variant marker kept so
/// the cli dispatcher's call sites do not need to be rewritten; the
/// only legitimate mode is `Vm`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunMode {
    /// Register-based bytecode VM.
    Vm,
}

/// `gos run` dispatcher: walks the project root for a default entry
/// point when no path is supplied.
pub(crate) fn dispatch(
    path: Option<PathBuf>,
    mode: RunMode,
    main_thread: bool,
    args: &[String],
) -> Result<()> {
    let resolved = resolve_entry_arg(path)?;
    run(&resolved, mode, main_thread, args)
}

fn run(file: &Path, _mode: RunMode, main_thread: bool, forwarded: &[String]) -> Result<()> {
    let file = file.to_path_buf();
    let forwarded = forwarded.to_vec();
    if main_thread {
        // Execute directly on the process main thread so native
        // libraries that require it (GLFW / OpenGL / Cocoa / Metal) work
        // from `[rust-bindings]`. Trades the large spawned-thread stack
        // for the OS-default main-thread stack (see `cmd::on_main_thread`).
        crate::cmd::on_main_thread(move || run_on_vm(&file, &forwarded))
    } else {
        // Execute on a thread with a large native stack so the host's
        // default main-thread stack size never bounds recursion depth or
        // the in-process JIT compile pass (see `cmd::with_vm_stack`).
        crate::cmd::with_vm_stack(move || run_on_vm(&file, &forwarded))
    }
}

fn run_on_vm(file: &PathBuf, forwarded: &[String]) -> Result<()> {
    let user_source = read_entry_source(file)?;
    // Compile-time codegen pass: synthesize `from_json` / `to_json`
    // for every user struct so the resulting program has real
    // methods (no VM-only intercept).
    let source = gossamer_parse::autoderive::augment_source(&user_source);
    // Comptime fold: evaluate `comptime { ... }` / `comptime fn` calls
    // now and splice their result literals in, so the VM compiles a
    // constant identical to what the compiled tiers see.
    let source = crate::comptime_fold::fold_comptime(&source, &file.to_string_lossy())?;
    profile_rss_stage("source_prepared");
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    // Static checks always run first. A program with parse / resolve /
    // type errors has no business reaching the VM - execution would
    // either crash or produce unsound output.
    let (program, tcx) = load_and_check(&source, file_id, &map)?;
    // The VM keeps the HIR and type context it needs for bytecode/JIT loading,
    // but it neither renders source diagnostics nor consults the SourceMap at
    // runtime. Release the augmented source and parse-time map before the VM
    // is created so their peak does not overlap bytecode chunks, MIR, and a
    // deferred Cranelift artifact. Runtime tracebacks use the VM call stack,
    // not this compile-time map.
    drop(map);
    drop(source);
    profile_rss_stage("frontend_released");
    gossamer_interp::set_program_name(&file.to_string_lossy());
    gossamer_interp::set_program_args(forwarded);
    let mut vm = gossamer_interp::Vm::new();
    profile_rss_stage("vm_created");
    // `load` consumes `tcx` (moves the interner into the JIT snapshot).
    vm.load(&program, tcx, true)
        .map_err(|err| anyhow!("vm load failed: {err}"))?;
    profile_rss_stage("vm_loaded");
    drop(program);
    let r = vm.call("main", Vec::new());
    profile_rss_stage("execution_complete");
    vm.release_jit_prelude();
    gossamer_interp::join_outstanding_goroutines();
    gossamer_interp::flush_runtime_stdout();
    match r {
        Ok(val) => {
            // An entry point returning `Err(e)` - an explicit `fn main() ->
            // Result<..>`, or the implicit `?`-desugared top-level main -
            // reports the error's Display (the colon-joined cause chain) to
            // stderr and exits nonzero, matching the native tier's
            // `gos_rt_main_exit_code_err`. Previously the return value was
            // discarded, so the error was silent and the exit code was 0.
            if let gossamer_interp::Value::Variant(inner) = &val
                && inner.name == "Err"
            {
                if let Some(payload) = inner.fields.first() {
                    eprintln!("{payload}");
                }
                std::process::exit(1);
            }
            Ok(())
        }
        Err(err) => {
            let trace = crate::cmd::traceback::render_call_stack(&vm.call_stack_snapshot());
            if gossamer_interp::is_panic_error(&err) {
                // A user hook replaces the default report; either way a
                // main-goroutine panic exits with the pinned code 101
                // (Rust parity - scripts depend on it).
                let hooked = vm.invoke_panic_hook(&gossamer_interp::panic_message(&err));
                if !hooked {
                    eprintln!("error: runtime error: {err}{trace}");
                }
                gossamer_interp::flush_runtime_stdout();
                std::process::exit(101);
            }
            Err(anyhow!("runtime error: {err}{trace}"))
        }
    }
}
