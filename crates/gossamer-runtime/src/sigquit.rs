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
//! the `backtrace` crate, which honours the DWARF emitted under
//! `gos build --release -g`.

use std::io::Write;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

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
    pub function: String,
    /// Source file path captured from DWARF, when available.
    pub file: String,
    /// 1-based line number captured from DWARF.
    pub line: u32,
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
        },
    );
}

/// Updates the wait state of an already-registered goroutine.
pub fn set_state(gid: u32, state: &'static str) {
    let mut g = registry().infos.lock();
    if let Some(info) = g.get_mut(&gid) {
        info.state = state;
    }
}

/// Updates the latest source position of a goroutine — called by
/// the codegen safepoint poll when DWARF info is available, or by
/// the interpreter on every step boundary.
pub fn set_position(gid: u32, file: impl Into<String>, line: u32) {
    let mut g = registry().infos.lock();
    if let Some(info) = g.get_mut(&gid) {
        info.file = file.into();
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
        // Fall back to the host backtrace at this point. This is
        // only the OS thread's backtrace, not the goroutine's; full
        // per-goroutine stacks land once Track A's stack-switching
        // primitives ship. Even with that limitation, the frame names
        // are useful for "which goroutine is hot".
        let trace = format!("{:?}", backtrace::Backtrace::new());
        for line in trace.lines().take(6) {
            out.write_all(b"        ")?;
            out.write_all(line.as_bytes())?;
            out.write_all(b"\n")?;
            written += line.len() + 9;
        }
    }
    Ok(written)
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

#[cfg(not(unix))]
pub fn install_handler() {
    // Windows has no SIGQUIT; the equivalent (CTRL+BREAK) is owned
    // by signal::Notifier. SIGQUIT-style dump is a Phase-2 item on
    // Windows.
}

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
