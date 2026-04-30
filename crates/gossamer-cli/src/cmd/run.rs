//! `gos run [PATH]` — execute a program through the VM (default)
//! or the tree-walker interpreter (`--tree-walker`).
//!
//! Both paths route through `loaders::load_and_check` so a
//! statically-invalid program never reaches execution.

use std::path::PathBuf;

use anyhow::{Result, anyhow};

use crate::loaders::load_and_check;
use crate::paths::{default_main_entry, read_source};

/// How `gos run` executes a program.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunMode {
    /// Default: register-based bytecode VM. Silently falls back
    /// to the tree-walker when the VM compiler hits an HIR
    /// construct it doesn't yet lower (closures-with-late-binding,
    /// etc.).
    Vm,
    /// `--tree-walker`: force the tree-walker. Slower but covers
    /// every construct; useful for debugging the VM or chasing
    /// parity differences.
    TreeWalker,
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

fn run(file: &PathBuf, mode: RunMode, forwarded: &[String]) -> Result<()> {
    let source = read_source(file)?;
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    // Static checks always run first, regardless of execution
    // mode. A program with parse / resolve / type errors has no
    // business reaching the VM — execution would either crash
    // or produce unsound output.
    let (program, mut tcx) = load_and_check(&source, file_id, &map)?;
    gossamer_interp::set_program_args(forwarded);
    if mode == RunMode::TreeWalker {
        return run_tree_walker(&program);
    }
    // Default: VM with tree-walker fallback. Load failure usually
    // means the VM compiler refused an HIR shape; the tree-walker
    // covers the long tail.
    let mut vm = gossamer_interp::Vm::new();
    match vm.load(&program, &mut tcx) {
        Ok(()) => {
            let r = vm.call("main", Vec::new()).map(|_| ());
            // JIT-promoted bodies print through the runtime's
            // thread-local `STDOUT_BUF` rather than the bytecode
            // VM's writer. Drain the buffer so any output that
            // bypassed the bytecode path still reaches the user
            // before we exit.
            gossamer_interp::flush_runtime_stdout();
            match r {
                Ok(()) => Ok(()),
                Err(err) => {
                    // VM runtime error must NOT silently re-run via
                    // the tree-walker — the program has already
                    // emitted side effects (println, file I/O); a
                    // re-run would duplicate them. Surface the
                    // error with whatever call-stack snapshot the
                    // VM has tracked.
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
        Err(err) => {
            if std::env::var("GOS_VM_TRACE").is_ok() {
                eprintln!("vm load failed ({err}); falling back to tree-walker");
            }
            run_tree_walker(&program)
        }
    }
}

fn run_tree_walker(program: &gossamer_hir::HirProgram) -> Result<()> {
    let mut interp = gossamer_interp::Interpreter::new();
    interp.load(program);
    let result = interp.call("main", Vec::new());
    gossamer_interp::join_outstanding_goroutines();
    if let Err(err) = result {
        let stack = interp.call_stack();
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
        return Err(anyhow!("runtime error: {err}{trace}"));
    }
    Ok(())
}
