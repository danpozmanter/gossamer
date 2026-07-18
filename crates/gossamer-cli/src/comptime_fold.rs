//! Comptime fold pass.
//!
//! `comptime { ... }` blocks and `comptime fn` calls are evaluated on
//! the bytecode VM during compilation and spliced back into the source
//! as literals. Running once here, ahead of the per-tier pipelines,
//! guarantees the bytecode VM, the Cranelift JIT, and the LLVM AOT
//! backend all compile the identical constant - comptime never reaches
//! a backend.
//!
//! The evaluation-and-splice core is `gossamer_interp::fold_into_source`
//! (shared with the wasm playground); this wrapper runs the front-end
//! gate over the already-augmented source and hands the lowered program
//! to the core. Programs with no `comptime` spelling skip the whole
//! pass.

use anyhow::{Result, anyhow};

/// Evaluates every comptime region in `augmented` (autoderive-augmented
/// source) and returns the source with each region replaced by its
/// result literal. Returns `augmented` unchanged when it contains no
/// `comptime` spelling, or when the front-end gate rejects the program
/// (the caller's real pass re-runs the gate and reports those errors).
/// Returns `Err` when a comptime region is not compile-time-known or
/// does not evaluate to a scalar or string.
pub(crate) fn fold_comptime(augmented: String, file_label: &str) -> Result<String> {
    if !augmented.contains("comptime") {
        return Ok(augmented);
    }

    // Comptime evaluation runs the bytecode VM during compilation, whose
    // native dispatch and in-process JIT grow the real machine stack just
    // as `gos run` does. Run it on a VM-sized stack so the host
    // main-thread stack (roughly 1 MiB on Windows) never bounds comptime
    // recursion: the `build` and `check` paths reach here on the main
    // thread, unlike `run` / `test`, which already execute inside
    // `with_vm_stack`.
    let file_label = file_label.to_string();
    crate::cmd::with_vm_stack(move || fold_comptime_on_vm(augmented, file_label))
}

/// Evaluates the comptime regions of `augmented` on the current thread,
/// returning the spliced source. Always invoked through
/// [`crate::cmd::with_vm_stack`] so the bytecode VM has a generous native
/// stack regardless of which command (`build` / `check` / `run` / `test`)
/// reached the fold.
fn fold_comptime_on_vm(augmented: String, file_label: String) -> Result<String> {
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file_label.clone(), augmented.clone());
    let outcome = gossamer_driver::check_frontend(&augmented, file_id);
    if !outcome.is_ok() {
        // Other front-end errors exist; let the caller's authoritative
        // gate render them rather than masking them behind a comptime
        // failure.
        return Ok(augmented);
    }

    let gossamer_driver::CheckedFrontend {
        edition: _,
        sf,
        resolutions,
        table,
        mut tcx,
    } = outcome.checked;
    let program = gossamer_hir::lower_source_file(&sf, &resolutions, &table, &mut tcx);

    gossamer_interp::fold_into_source(&program, tcx, &augmented, &file_label)
        .map_err(|message| anyhow!(message))
}
