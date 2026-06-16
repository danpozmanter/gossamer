//! `gos run [PATH]` - execute a program through the bytecode VM.
//!
//! The register-based bytecode VM is the only execution engine for
//! `gos run`. The tree-walker interpreter was removed in 0.14.0 (the
//! `--tree-walker` flag in 0.5.0); no CLI / env-var path selects an
//! engine.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::loaders::load_and_check;
use crate::paths::{default_main_entry, read_entry_source};

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
pub(crate) fn dispatch(path: Option<PathBuf>, mode: RunMode, args: &[String]) -> Result<()> {
    let resolved = match path {
        Some(p) => p,
        None => default_main_entry()?,
    };
    run(&resolved, mode, args)
}

fn run(file: &Path, _mode: RunMode, forwarded: &[String]) -> Result<()> {
    // Execute on a thread with a large native stack so the host's
    // default main-thread stack size never bounds recursion depth or
    // the in-process JIT compile pass (see `cmd::with_vm_stack`).
    let file = file.to_path_buf();
    let forwarded = forwarded.to_vec();
    crate::cmd::with_vm_stack(move || run_on_vm(&file, &forwarded))
}

fn run_on_vm(file: &PathBuf, forwarded: &[String]) -> Result<()> {
    let user_source = read_entry_source(file)?;
    // Compile-time codegen pass: synthesize `from_json` / `to_json`
    // for every user struct so the resulting program has real
    // methods (no VM-only intercept).
    let source = gossamer_parse::autoderive::augment_source(&user_source);
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    // Static checks always run first. A program with parse / resolve /
    // type errors has no business reaching the VM - execution would
    // either crash or produce unsound output.
    let (program, tcx) = load_and_check(&source, file_id, &map)?;
    gossamer_interp::set_program_name(&file.to_string_lossy());
    gossamer_interp::set_program_args(forwarded);
    let mut vm = gossamer_interp::Vm::new();
    // `load` consumes `tcx` (moves the interner into the JIT snapshot).
    vm.load(&program, tcx, true)
        .map_err(|err| anyhow!("vm load failed: {err}"))?;
    drop(program);
    let r = vm.call("main", Vec::new()).map(|_| ());
    vm.release_jit_prelude();
    gossamer_interp::join_outstanding_goroutines();
    gossamer_interp::flush_runtime_stdout();
    match r {
        Ok(()) => Ok(()),
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
