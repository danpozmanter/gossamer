//! `gos run [PATH]` — execute a program through the bytecode VM.
//!
//! The bytecode VM is the only interpretation tier. The tree-walker
//! was retired in 0.5.0; it is no longer reachable from this command
//! or from any CLI / env-var path.

use std::path::PathBuf;

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

fn run(file: &PathBuf, _mode: RunMode, forwarded: &[String]) -> Result<()> {
    let user_source = read_entry_source(file)?;
    // Compile-time codegen pass: synthesize `from_json` / `to_json`
    // for every user struct so the resulting program has real
    // methods (no VM-only intercept).
    let source = gossamer_parse::autoderive::augment_source(&user_source);
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    // Static checks always run first. A program with parse / resolve /
    // type errors has no business reaching the VM — execution would
    // either crash or produce unsound output.
    let (program, mut tcx) = load_and_check(&source, file_id, &map)?;
    gossamer_interp::set_program_name(&file.to_string_lossy());
    gossamer_interp::set_program_args(forwarded);
    let mut vm = gossamer_interp::Vm::new();
    vm.load(&program, &mut tcx)
        .map_err(|err| anyhow!("vm load failed: {err}"))?;
    drop(program);
    drop(tcx);
    let r = vm.call("main", Vec::new()).map(|_| ());
    vm.release_jit_prelude();
    gossamer_interp::join_outstanding_goroutines();
    gossamer_interp::flush_runtime_stdout();
    match r {
        Ok(()) => Ok(()),
        Err(err) => {
            let stack = vm.call_stack_snapshot();
            let trace = if stack.is_empty() {
                String::new()
            } else {
                let mut rendered = String::from("\n  call stack (outermost first):");
                for name in &stack {
                    rendered.push_str("\n    at ");
                    rendered.push_str(name);
                }
                rendered
            };
            Err(anyhow!("runtime error: {err}{trace}"))
        }
    }
}
