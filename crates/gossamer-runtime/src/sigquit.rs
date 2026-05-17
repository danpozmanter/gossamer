//! SIGQUIT goroutine stack dump.
//!
//! Pressing Ctrl-\ on a Gossamer process (or sending it SIGQUIT)
//! prints a Go-format dump of every live goroutine's stack to
//! stderr, then exits non-zero. This is the single most useful
//! production-incident diagnostic — without it, a hung service
//! is opaque.
//!
//! The handler runs on a dedicated relay thread (signal-hook's
//! safe abstraction over `sigaction`), so the printing logic is
//! free to allocate / take locks. Ordinary signal-handler async-
//! safety constraints don't apply.
//!
//! Output format mirrors Go's runtime stack dump closely enough
//! that existing tools (`stackparse`, `goroutine-stack-summarizer`,
//! grep) read it without modification:
//!
//! ```text
//! goroutine 17 [running]:
//!   main.handle_request(0xdeadbeef, 42)
//!           /path/to/main.gos:128 +0x4c
//!   main.main()
//!           /path/to/main.gos:18 +0x12
//!
//! goroutine 18 [chan receive]:
//!   ...
//! ```
//!
//! The address-only frame (`+0x4c` style) is filled in if DWARF is
//! available; otherwise the line falls back to a decimal byte
//! offset from the function entry. Backtrace symbolication uses
//! `std::backtrace::Backtrace::capture()`, which honours the DWARF
//! emitted under `gos build --release -g`. Using the std API
//! instead of the standalone `backtrace` crate keeps `libgcc_s` out
//! of the dependency closure, which is a precondition for the
//! static-musl link path on Linux.

use std::io::Write;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

/// One frame on a goroutine's call stack. Pushed on every function
/// entry by [`stack_push`] and popped on return by [`stack_pop`].
#[derive(Debug, Clone)]
pub struct Frame {
    /// Symbolicated function name (e.g. `main::handle_request`).
    pub function: String,
    /// Source file path. Empty if no debug info is available.
    pub file: String,
    /// 1-based source line of the most recent statement executed
    /// in this frame. Updated by [`set_position`] at MIR-statement
    /// granularity.
    pub line: u32,
}

/// Per-goroutine record published into the runtime's introspection
/// table. The scheduler updates this on park / unpark / spawn /
/// finish; SIGQUIT handler walks the table to render the dump.
#[derive(Debug, Clone)]
pub struct GoroutineInfo {
    /// Stable goroutine identifier.
    pub gid: u32,
    /// Last-known wait reason (`"running"`, `"chan receive"`, ...).
    pub state: &'static str,
    /// Symbolicated function name the goroutine was last running in.
    /// Empty when no frame has been recorded yet.
    /// Mirrors `frames.last().function` for backward-compatible
    /// consumers that only need the topmost name.
    pub function: String,
    /// Source file path captured from DWARF, when available.
    /// Mirrors `frames.last().file`.
    pub file: String,
    /// 1-based line number captured from DWARF.
    /// Mirrors `frames.last().line`.
    pub line: u32,
    /// Full call stack (outermost frame first). Empty until the
    /// codegen-emitted prologue runs `gos_rt_stack_push`.
    pub frames: Vec<Frame>,
}

#[derive(Default)]
struct Registry {
    infos: Mutex<std::collections::BTreeMap<u32, GoroutineInfo>>,
    next_id: AtomicU64,
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

fn registry() -> &'static Registry {
    REGISTRY.get_or_init(Registry::default)
}

/// Allocates a fresh goroutine id for tracking purposes. Distinct
/// from the scheduler's `Gid` because that one wraps `u32` and we
/// want a wider counter for diagnostics in long-running processes.
#[must_use]
pub fn next_id() -> u32 {
    let raw = registry().next_id.fetch_add(1, Ordering::Relaxed);
    u32::try_from(raw & 0xFFFF_FFFF).unwrap_or(u32::MAX)
}

/// Publishes a fresh entry for `gid` (called when the scheduler
/// spawns a goroutine).
pub fn register(gid: u32, function: impl Into<String>) {
    let mut g = registry().infos.lock();
    g.insert(
        gid,
        GoroutineInfo {
            gid,
            state: "running",
            function: function.into(),
            file: String::new(),
            line: 0,
            frames: Vec::new(),
        },
    );
}

thread_local! {
    static ACTIVE_GID: std::cell::Cell<u32> = const { std::cell::Cell::new(u32::MAX) };
}

/// Sets the goroutine id associated with the current OS thread.
/// The scheduler calls this when resuming a goroutine, and the
/// interpreter / panic helpers read it to attribute frame pushes
/// and panic dumps to the right goroutine. `u32::MAX` is the
/// sentinel "no goroutine — main thread".
pub fn set_active_gid(gid: u32) {
    ACTIVE_GID.with(|cell| cell.set(gid));
}

/// Returns the goroutine id currently bound to this thread, or
/// `None` if no goroutine is running (main thread).
#[must_use]
pub fn active_gid() -> Option<u32> {
    ACTIVE_GID.with(|cell| {
        let v = cell.get();
        if v == u32::MAX { None } else { Some(v) }
    })
}

/// Pushes a new frame onto the active goroutine's call stack.
/// Called at function-entry safepoints by the codegen prologue
/// and by the interpreter on every call. Cheap (one allocation
/// per call; the hot path is the runtime's `register` table lock).
///
/// If no goroutine is active (main-thread bare execution), the
/// stack is kept under a dedicated `u32::MAX` slot so panic dumps
/// from `fn main` still get a useful trace.
pub fn stack_push(function: impl Into<String>, file: impl Into<String>, line: u32) {
    let gid = ACTIVE_GID.with(std::cell::Cell::get);
    let function = function.into();
    let file = file.into();
    let frame = Frame {
        function: function.clone(),
        file: file.clone(),
        line,
    };
    let mut g = registry().infos.lock();
    let info = g.entry(gid).or_insert_with(|| GoroutineInfo {
        gid,
        state: "running",
        function: function.clone(),
        file: file.clone(),
        line,
        frames: Vec::new(),
    });
    info.frames.push(frame);
    info.function = function;
    info.file = file;
    info.line = line;
}

/// Pops the topmost frame from the active goroutine's call stack.
/// Called at function-return safepoints by the codegen epilogue
/// and by the interpreter on every return / `?` propagation.
/// Tolerates over-pop (no-op when the stack is empty) so unwinding
/// past an aborted frame doesn't crash the runtime.
pub fn stack_pop() {
    let gid = ACTIVE_GID.with(std::cell::Cell::get);
    let mut g = registry().infos.lock();
    if let Some(info) = g.get_mut(&gid) {
        info.frames.pop();
        if let Some(top) = info.frames.last() {
            info.function.clone_from(&top.function);
            info.file.clone_from(&top.file);
            info.line = top.line;
        } else {
            info.function.clear();
            info.file.clear();
            info.line = 0;
        }
    }
}

/// Snapshots the active goroutine's call stack (outermost first).
/// Used by the panic helper to render the failing frame chain
/// inline with the diagnostic.
#[must_use]
pub fn active_frames() -> Vec<Frame> {
    let gid = ACTIVE_GID.with(std::cell::Cell::get);
    registry()
        .infos
        .lock()
        .get(&gid)
        .map(|info| info.frames.clone())
        .unwrap_or_default()
}

/// Updates the wait state of an already-registered goroutine.
pub fn set_state(gid: u32, state: &'static str) {
    let mut g = registry().infos.lock();
    if let Some(info) = g.get_mut(&gid) {
        info.state = state;
    }
}

/// Updates the line number of the topmost call-stack frame for
/// the active goroutine. Cheap (single locked map lookup); called
/// at MIR-statement granularity by codegen so panic traces carry
/// the precise failing line, not just the function-entry line.
pub fn set_active_line(line: u32) {
    let gid = ACTIVE_GID.with(std::cell::Cell::get);
    let mut g = registry().infos.lock();
    if let Some(info) = g.get_mut(&gid) {
        if let Some(top) = info.frames.last_mut() {
            top.line = line;
        }
        info.line = line;
    }
}

/// Updates the latest source position of a goroutine — called by
/// the codegen safepoint poll when DWARF info is available, or by
/// the interpreter on every step boundary. Also updates the
/// topmost call-stack frame's line so panic dumps show the line
/// of the most recent statement, not the function entry.
pub fn set_position(gid: u32, file: impl Into<String>, line: u32) {
    let mut g = registry().infos.lock();
    if let Some(info) = g.get_mut(&gid) {
        let file = file.into();
        if let Some(top) = info.frames.last_mut() {
            top.file.clone_from(&file);
            top.line = line;
        }
        info.file = file;
        info.line = line;
    }
}

/// Removes the entry when the goroutine finishes.
pub fn unregister(gid: u32) {
    registry().infos.lock().remove(&gid);
}

/// Snapshots every live goroutine. Used by the SIGQUIT handler
/// and by `runtime::all_goroutines()`.
#[must_use]
pub fn snapshot() -> Vec<GoroutineInfo> {
    registry().infos.lock().values().cloned().collect()
}

/// Renders a Go-style stack dump into a writer. Returns the number
/// of bytes written.
///
/// # Errors
///
/// Returns an error if the underlying writer fails.
pub fn render_to(out: &mut impl Write) -> std::io::Result<usize> {
    let mut written = 0;
    let infos = snapshot();
    let _ = writeln!(out, "SIGQUIT: dumping {} goroutine(s)", infos.len()).map(|()| written += 1);
    for info in infos {
        let header = format!(
            "\ngoroutine {gid} [{state}]:\n",
            gid = info.gid,
            state = info.state,
        );
        out.write_all(header.as_bytes())?;
        written += header.len();
        if info.frames.is_empty() {
            let func_line = if info.function.is_empty() {
                "  <unknown>()\n".to_string()
            } else {
                format!("  {}()\n", info.function)
            };
            out.write_all(func_line.as_bytes())?;
            written += func_line.len();
            if !info.file.is_empty() {
                let pos = format!(
                    "        {file}:{line}\n",
                    file = info.file,
                    line = info.line
                );
                out.write_all(pos.as_bytes())?;
                written += pos.len();
            }
            // No Gossamer frames — fall back to the host backtrace.
            // The first ~6 lines are enough to identify "which
            // goroutine is hot" without flooding the dump.
            let trace = std::backtrace::Backtrace::force_capture().to_string();
            for line in trace.lines().take(6) {
                out.write_all(b"        ")?;
                out.write_all(line.as_bytes())?;
                out.write_all(b"\n")?;
                written += line.len() + 9;
            }
        } else {
            // Render the full Gossamer call stack, innermost last
            // (matches Rust / Go convention — most recent call on
            // top, deepest call at the bottom near the panic).
            for frame in info.frames.iter().rev() {
                let func_line = format!("  {}()\n", frame.function);
                out.write_all(func_line.as_bytes())?;
                written += func_line.len();
                if !frame.file.is_empty() {
                    let pos = format!(
                        "        {file}:{line}\n",
                        file = frame.file,
                        line = frame.line
                    );
                    out.write_all(pos.as_bytes())?;
                    written += pos.len();
                }
            }
        }
    }
    Ok(written)
}

/// Renders just the active goroutine's call stack into a string,
/// innermost frame first. Used by [`gos_rt_panic`] to inline the
/// trace with the diagnostic.
#[must_use]
pub fn render_active_panic_trace() -> String {
    let frames = active_frames();
    if frames.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for frame in frames.iter().rev() {
        out.push_str("    at ");
        out.push_str(&frame.function);
        if !frame.file.is_empty() {
            out.push_str(" (");
            out.push_str(&frame.file);
            out.push(':');
            out.push_str(&frame.line.to_string());
            out.push(')');
        }
        out.push('\n');
    }
    out
}

/// Installs the SIGQUIT handler. Idempotent.
///
/// SIGQUIT delivery itself is owned by `gossamer_std::signal`'s
/// single blocking dispatcher thread — when it sees SIGQUIT, it
/// calls [`render_to`] directly. This entry point stays as a
/// no-op to preserve the `install_handler()` call sites that the
/// scheduler boot path uses.
#[cfg(unix)]
pub fn install_handler() {}

/// No-op on Windows — the `SetConsoleCtrlHandler` dispatcher in
/// `gossamer_std::signal` already calls [`render_to`] directly on
/// `CTRL_BREAK_EVENT`. Other non-unix targets have no equivalent.
#[cfg(not(unix))]
pub fn install_handler() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_round_trips_a_goroutine() {
        let gid = next_id();
        register(gid, "test::handle");
        set_state(gid, "chan receive");
        set_position(gid, "main.gos", 42);
        let snap = snapshot();
        let entry = snap
            .iter()
            .find(|info| info.gid == gid)
            .expect("registered entry");
        assert_eq!(entry.state, "chan receive");
        assert_eq!(entry.function, "test::handle");
        assert_eq!(entry.line, 42);
        unregister(gid);
        assert!(!snapshot().iter().any(|info| info.gid == gid));
    }

    #[test]
    fn render_to_writes_some_output() {
        let gid = next_id();
        register(gid, "test::handle");
        let mut buf = Vec::new();
        let n = render_to(&mut buf).unwrap();
        assert!(n > 0);
        let s = String::from_utf8_lossy(&buf);
        assert!(s.contains("goroutine"));
        assert!(s.contains("test::handle"));
        unregister(gid);
    }
}
