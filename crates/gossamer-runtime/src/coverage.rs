//! Line + branch coverage instrumentation table.
//!
//! Each unique `(file, line)` pair compiled while coverage is
//! enabled gets an `AtomicU64` counter. Codegen and the interp
//! bump the counter at every basic-block entry; the test runner
//! snapshots the table at exit and renders an lcov report.
//!
//! The table is lazily populated — registering an `(file, line)`
//! that hasn't been seen yet appends a slot. Reads from the table
//! are O(1) via an indirection through a `(file, line) -> idx`
//! map. The hot path (bump) is a single `fetch_add`.
//!
//! When coverage is off (default), `bump` is still a single load
//! of the global enable flag plus an early return; the runtime
//! cost is negligible for programs not running under `gos test
//! --coverage`.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use parking_lot::RwLock;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// One coverage counter and its source location.
#[derive(Debug)]
pub struct Counter {
    /// Source file path.
    pub file: String,
    /// 1-based source line.
    pub line: u32,
    /// Optional branch index (`0` = no branch / sequential
    /// statement, `1..N` = arm of a `match` / branch of an `if`).
    pub branch: u32,
    /// Hit counter — bumped on every BB entry.
    pub hits: AtomicU64,
}

#[derive(Default)]
struct Table {
    counters: RwLock<Vec<Counter>>,
    /// `(file, line, branch)` -> counters index. Held under the
    /// same `RwLock` as the counters Vec to keep registration
    /// atomic.
    index: RwLock<HashMap<(String, u32, u32), usize>>,
}

static TABLE: OnceLock<Table> = OnceLock::new();

fn table() -> &'static Table {
    TABLE.get_or_init(Table::default)
}

/// Enables (or disables) coverage instrumentation. The test
/// runner flips this on before invoking the program under
/// `gos test --coverage`.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Release);
}

/// `true` when coverage instrumentation is requested.
#[must_use]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

/// Registers a counter slot for `(file, line, branch)` and
/// returns its index. Idempotent — repeated calls with the same
/// triple return the same index.
#[must_use]
pub fn register(file: &str, line: u32, branch: u32) -> usize {
    let key = (file.to_string(), line, branch);
    if let Some(&idx) = table().index.read().get(&key) {
        return idx;
    }
    let mut index = table().index.write();
    if let Some(&idx) = index.get(&key) {
        return idx;
    }
    let mut counters = table().counters.write();
    let idx = counters.len();
    counters.push(Counter {
        file: file.to_string(),
        line,
        branch,
        hits: AtomicU64::new(0),
    });
    index.insert(key, idx);
    idx
}

/// Bumps the hit counter at `idx`. Cheap (one `fetch_add`); the
/// codegen-emitted call site does `if enabled() { bump(idx) }`
/// so the cost is amortised across the global flag check.
pub fn bump(idx: usize) {
    if !enabled() {
        return;
    }
    let counters = table().counters.read();
    if let Some(c) = counters.get(idx) {
        c.hits.fetch_add(1, Ordering::Relaxed);
    }
}

/// Convenience entry combining [`register`] + [`bump`]. Used by
/// the interpreter where the registration cost is amortised
/// across the per-statement loop without a separate prepass.
///
/// Returns the slot index so the caller can cache it.
#[must_use]
pub fn record(file: &str, line: u32, branch: u32) -> usize {
    let idx = register(file, line, branch);
    bump(idx);
    idx
}

/// A snapshot of one counter for the report writer.
#[derive(Debug, Clone)]
pub struct CounterSnapshot {
    /// Source file path.
    pub file: String,
    /// 1-based source line.
    pub line: u32,
    /// Branch index (0 for sequential statements).
    pub branch: u32,
    /// Hit count.
    pub hits: u64,
}

/// Snapshots the current counter table. The test runner calls
/// this at exit to render the lcov report.
#[must_use]
pub fn snapshot() -> Vec<CounterSnapshot> {
    table()
        .counters
        .read()
        .iter()
        .map(|c| CounterSnapshot {
            file: c.file.clone(),
            line: c.line,
            branch: c.branch,
            hits: c.hits.load(Ordering::Relaxed),
        })
        .collect()
}

/// Resets every counter back to zero. The test runner calls this
/// between independent test runs so cumulative counts don't bleed
/// across runs.
pub fn reset() {
    for c in table().counters.read().iter() {
        c.hits.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_round_trips_and_dedups() {
        let a = register("foo.gos", 10, 0);
        let b = register("foo.gos", 10, 0);
        assert_eq!(a, b);
        let c = register("foo.gos", 11, 0);
        assert_ne!(a, c);
    }

    #[test]
    fn bump_increments_only_when_enabled() {
        reset();
        set_enabled(false);
        let idx = register("bar.gos", 5, 0);
        bump(idx);
        let snap = snapshot();
        let entry = snap
            .iter()
            .find(|c| c.file == "bar.gos" && c.line == 5)
            .unwrap();
        assert_eq!(entry.hits, 0);

        set_enabled(true);
        bump(idx);
        bump(idx);
        let snap = snapshot();
        let entry = snap
            .iter()
            .find(|c| c.file == "bar.gos" && c.line == 5)
            .unwrap();
        assert_eq!(entry.hits, 2);
        set_enabled(false);
        reset();
    }
}
