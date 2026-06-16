//! Blocking-pool dispatch shim for `#[gos_blocking]`.
//!
//! A binding fn annotated with `#[gos_blocking]` runs its body
//! through [`run_blocking`], which on platforms with a scheduler
//! blocking pool routes the work to a dedicated OS thread so the
//! calling goroutine yields cleanly. On platforms without one
//! (or before the scheduler hook is wired in this build), the
//! function runs inline - observable behaviour is identical, the
//! only difference is whether the scheduler can keep other
//! goroutines making progress.

/// Run `f` to completion. On supported tiers this dispatches to a
/// dedicated blocking-pool thread and parks the calling goroutine;
/// otherwise it runs inline.
///
/// The fallback (inline) behaviour is sound: every binding fn body
/// was originally written assuming sync execution, so running it on
/// the calling thread cannot break correctness - it only forgoes
/// the scheduler-yielding benefit. Wiring through to a real pool is
/// a runtime concern tracked in `~/dev/contexts/lang/ffi.md` and
/// `~/dev/contexts/gos/ecosystem.md` (SQLite, tonic, sync HTTP).
pub fn run_blocking<F, R>(f: F) -> R
where
    F: FnOnce() -> R + Send,
    R: Send,
{
    f()
}
