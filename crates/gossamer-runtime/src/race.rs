//! Race detector runtime support.
//!
//! When the program is compiled with `--race`, the codegen emits a
//! call to [`record_access`] before every heap-pointer load /
//! store. This module's tracker maintains a happens-before model
//! built from the scheduler's park / unpark events plus
//! Mutex / Channel synchronisation events:
//!
//! - Per-goroutine vector clock (`Vec<u64>` indexed by goroutine id).
//! - Per-address last-access record `(gid, op, vector_clock)`.
//! - On every access, the tracker compares the current goroutine's
//!   clock against the last recorded access; if neither happens
//!   before the other and at least one is a write, a data race is
//!   reported.
//!
//! The tracker shipped here is the foundation: the vector-clock
//! propagation hooks for Mutex/Channel events are in place, and
//! a simple write-write race is detected and reported. The full
//! `ThreadSanitizer` parity (slot map for memory regions, RAW/WAR
//! distinction for inter-goroutine ordering, lock-set analysis)
//! lands in Phase 2 — see `record_access` for what's shipping
//! today.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use rustc_hash::FxHashMap;

/// One observed access. Stored per address so the tracker can
/// reason about read-write / write-write conflicts.
#[derive(Debug, Clone)]
struct Access {
    gid: u32,
    write: bool,
    /// Frozen vector clock at the time of the access — keyed by
    /// goroutine id, value = the local logical step counter.
    clock: Vec<u64>,
}

#[derive(Default)]
struct Tracker {
    /// Per-goroutine logical clock.
    goroutines: Mutex<FxHashMap<u32, Vec<u64>>>,
    /// Last access seen for each address.
    accesses: Mutex<FxHashMap<usize, Access>>,
    /// Append-only race log; each entry is a human-readable
    /// description that `gos test --race` prints at the end of a
    /// run.
    races: Mutex<Vec<String>>,
}

static TRACKER: OnceLock<Tracker> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);

fn tracker() -> &'static Tracker {
    TRACKER.get_or_init(Tracker::default)
}

/// Activates the race detector. Called by `gos test --race` early
/// in `main`. While disabled, every entry point is a no-op.
pub fn enable() {
    ENABLED.store(true, Ordering::Release);
    let _ = tracker();
}

/// Returns `true` when the detector is active.
#[must_use]
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

fn ensure_clock_for(gid: u32) -> Vec<u64> {
    let mut g = tracker().goroutines.lock();
    g.entry(gid)
        .or_insert_with(|| {
            let len = (gid as usize + 1).max(8);
            let mut v = vec![0u64; len];
            v[gid as usize] = 1;
            v
        })
        .clone()
}

fn bump_clock(gid: u32) {
    let mut g = tracker().goroutines.lock();
    let entry = g.entry(gid).or_default();
    while entry.len() <= gid as usize {
        entry.push(0);
    }
    entry[gid as usize] += 1;
}

/// Records a memory access. `addr` is the heap address being
/// touched; `write` distinguishes load (false) from store (true).
/// Reports a race when the previous access is unsynchronised.
pub fn record_access(gid: u32, addr: usize, write: bool) {
    if !is_enabled() {
        return;
    }
    bump_clock(gid);
    let clock = ensure_clock_for(gid);
    let mut accesses = tracker().accesses.lock();
    if let Some(prev) = accesses.get(&addr).cloned() {
        if prev.gid != gid && (prev.write || write) && !happens_before(&prev.clock, &clock, gid) {
            let msg = format!(
                "DATA RACE: addr={addr:#x} prev={prev_gid} ({prev_op}) curr={gid} ({op})",
                prev_gid = prev.gid,
                prev_op = if prev.write { "write" } else { "read" },
                op = if write { "write" } else { "read" },
            );
            tracker().races.lock().push(msg);
        }
    }
    accesses.insert(addr, Access { gid, write, clock });
}

/// Records a synchronisation event between two goroutines: when
/// `from` releases (e.g. mutex unlock, channel send) and `to`
/// acquires the same primitive, `to`'s clock takes the
/// element-wise max of its own and `from`'s clocks, recording
/// that everything `from` saw now happens-before `to`.
pub fn record_sync(from: u32, to: u32) {
    if !is_enabled() {
        return;
    }
    let from_clock = ensure_clock_for(from);
    let mut g = tracker().goroutines.lock();
    let to_clock = g.entry(to).or_default();
    while to_clock.len() < from_clock.len() {
        to_clock.push(0);
    }
    let len = from_clock.len();
    for i in 0..len {
        if from_clock[i] > to_clock[i] {
            to_clock[i] = from_clock[i];
        }
    }
}

/// `true` when `prev` happens-before `curr` from `curr_gid`'s
/// perspective. The classic vector-clock ordering test.
fn happens_before(prev: &[u64], curr: &[u64], _curr_gid: u32) -> bool {
    let len = prev.len().min(curr.len());
    let mut strictly_less = false;
    for i in 0..len {
        if prev[i] > curr[i] {
            return false;
        }
        if prev[i] < curr[i] {
            strictly_less = true;
        }
    }
    if curr.len() > prev.len() {
        for &v in &curr[len..] {
            if v > 0 {
                strictly_less = true;
                break;
            }
        }
    }
    strictly_less
}

/// Drains the race log. Returns one human-readable line per
/// detected race. `gos test --race` prints these at the end of a
/// run and exits non-zero when the list is non-empty.
#[must_use]
pub fn drain_races() -> Vec<String> {
    let mut g = tracker().races.lock();
    std::mem::take(&mut *g)
}

/// C-ABI entry the codegen calls before every heap-pointer
/// load/store under `--race`. The `goroutine_id_thread_local`
/// helper supplies the current goroutine id; for now we read it
/// from the SIGQUIT registry's per-thread cache.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_race_access(addr: usize, write: i32) {
    if !is_enabled() {
        return;
    }
    record_access(current_gid(), addr, write != 0);
}

/// Returns the goroutine id for the current OS thread. Falls back
/// to `0` when no goroutine is registered for this thread (e.g.
/// the program's main thread before scheduler boot). The full
/// implementation reads from a thread-local published by
/// the scheduler when it dispatches a task; for now we use a
/// conservative `0` fallback that still surfaces races between
/// distinct goroutines but loses precision when many goroutines
/// share `gid 0`.
fn current_gid() -> u32 {
    CURRENT_GID.with(std::cell::Cell::get)
}

thread_local! {
    static CURRENT_GID: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Publishes the current goroutine's id into the per-thread
/// cache. Called by the scheduler when it dispatches a task and
/// when a goroutine is parked/unparked.
pub fn set_current_gid(gid: u32) {
    CURRENT_GID.with(|c| c.set(gid));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Race-detector tests share a process-wide enable flag and
    /// accumulator; serialising them avoids cross-test pollution.
    static TEST_GUARD: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    #[test]
    fn detector_finds_unsynchronised_write_write_race() {
        let _g = TEST_GUARD.lock();
        enable();
        let _ = drain_races();
        // Two goroutines write the same address with no
        // synchronisation: race expected.
        record_access(101, 0xCAFE, true);
        record_access(102, 0xCAFE, true);
        let races = drain_races();
        assert!(
            !races.is_empty(),
            "expected at least one race, got {races:?}"
        );
    }

    #[test]
    fn detector_does_not_flag_synchronised_writes() {
        let _g = TEST_GUARD.lock();
        enable();
        let _ = drain_races();
        // Goroutine 103 writes, hands off via record_sync to 104
        // which also writes — no race because the synchronisation
        // event makes 103's write happen-before 104's.
        record_access(103, 0xBEEF, true);
        record_sync(103, 104);
        record_access(104, 0xBEEF, true);
        let races = drain_races();
        assert!(
            races.is_empty(),
            "synchronised writes flagged as race: {races:?}"
        );
    }

    #[test]
    fn detector_is_noop_when_disabled() {
        let _g = TEST_GUARD.lock();
        ENABLED.store(false, Ordering::Release);
        let _ = drain_races();
        record_access(105, 0xDEAD, true);
        record_access(106, 0xDEAD, true);
        assert!(drain_races().is_empty());
    }
}
