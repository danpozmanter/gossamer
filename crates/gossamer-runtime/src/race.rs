//! Race detector runtime support.
//!
//! When the program is compiled with `--race`, the codegen emits a
//! call to [`record_access`] before every heap-pointer load /
//! store. This module's tracker maintains a happens-before model
//! built from the scheduler's park / unpark events plus
//! Mutex / Channel synchronisation events:
//!
//! - Per-goroutine vector clock (`Vec<u64>` indexed by goroutine id).
//! - Per-address state: the last write and up to [`MAX_ACTIVE_READS`]
//!   concurrent reads not yet dominated by a subsequent write.
//! - On every write, the tracker checks all active readers (WAR) and
//!   the last writer (WW) for unsynchronised conflicts. On every read,
//!   it checks the last writer (RAW). Any conflict where neither clock
//!   happens-before the other is reported.
//!
//! Older reads are evicted when `MAX_ACTIVE_READS` is exceeded so
//! the per-address state stays bounded. Lock-set analysis remains
//! out of scope.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use rustc_hash::FxHashMap;

/// At most this many concurrent read accesses are retained per
/// address. Oldest entries are evicted when the cap is reached.
const MAX_ACTIVE_READS: usize = 4;

/// One observed access. Stored per address so the tracker can
/// reason about read-write / write-write conflicts.
#[derive(Debug, Clone)]
struct Access {
    gid: u32,
    /// Frozen vector clock at the time of the access — keyed by
    /// goroutine id, value = the local logical step counter.
    clock: Vec<u64>,
}

/// Per-address access state: the last write and up to
/// [`MAX_ACTIVE_READS`] concurrent reads not yet dominated by a
/// subsequent write.
#[derive(Debug, Default)]
struct AddressState {
    last_write: Option<Access>,
    active_reads: Vec<Access>,
}

#[derive(Default)]
struct Tracker {
    /// Per-goroutine logical clock.
    goroutines: Mutex<FxHashMap<u32, Vec<u64>>>,
    /// Per-address write/read state.
    accesses: Mutex<FxHashMap<usize, AddressState>>,
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
///
/// On a write, checks all active readers for WAR conflicts and the
/// last writer for WW conflicts, then resets the address state.
/// On a read, checks the last writer for RAW conflicts, then appends
/// to the active reader set (oldest evicted when the cap is hit).
pub fn record_access(gid: u32, addr: usize, write: bool) {
    if !is_enabled() {
        return;
    }
    bump_clock(gid);
    let clock = ensure_clock_for(gid);
    let mut accesses = tracker().accesses.lock();
    let state = accesses.entry(addr).or_default();
    let mut new_races: Vec<String> = Vec::new();
    if write {
        // Write: check concurrent reads (WAR) then last write (WW).
        for reader in &state.active_reads {
            if reader.gid != gid && !happens_before(&reader.clock, &clock, gid) {
                new_races.push(format!(
                    "DATA RACE: addr={addr:#x} reader={} (read) writer={gid} (write)",
                    reader.gid,
                ));
            }
        }
        if let Some(ref prev_write) = state.last_write {
            if prev_write.gid != gid && !happens_before(&prev_write.clock, &clock, gid) {
                new_races.push(format!(
                    "DATA RACE: addr={addr:#x} prev={} (write) curr={gid} (write)",
                    prev_write.gid,
                ));
            }
        }
        state.active_reads.clear();
        state.last_write = Some(Access { gid, clock });
    } else {
        // Read: check last write (RAW) then append to active readers.
        if let Some(ref prev_write) = state.last_write {
            if prev_write.gid != gid && !happens_before(&prev_write.clock, &clock, gid) {
                new_races.push(format!(
                    "DATA RACE: addr={addr:#x} writer={} (write) reader={gid} (read)",
                    prev_write.gid,
                ));
            }
        }
        if state.active_reads.len() >= MAX_ACTIVE_READS {
            state.active_reads.remove(0);
        }
        state.active_reads.push(Access { gid, clock });
    }
    drop(accesses);
    if !new_races.is_empty() {
        tracker().races.lock().extend(new_races);
    }
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
/// the program's main thread before scheduler boot).
#[must_use]
pub fn current_gid() -> u32 {
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
        record_access(101, 0xCAFE, true);
        record_access(102, 0xCAFE, true);
        let races = drain_races();
        assert!(!races.is_empty(), "expected WW race, got {races:?}");
    }

    #[test]
    fn detector_finds_raw_race_write_then_read_no_sync() {
        let _g = TEST_GUARD.lock();
        enable();
        let _ = drain_races();
        // Goroutine 110 writes, goroutine 111 reads with no
        // synchronisation between them: read-after-write race.
        record_access(110, 0xAAAA, true);
        record_access(111, 0xAAAA, false);
        let races = drain_races();
        assert!(!races.is_empty(), "expected RAW race, got {races:?}");
        assert!(
            races
                .iter()
                .any(|r| r.contains("writer=110") && r.contains("reader=111")),
            "race message should name writer and reader: {races:?}",
        );
    }

    #[test]
    fn detector_finds_war_race_read_then_write_no_sync() {
        let _g = TEST_GUARD.lock();
        enable();
        let _ = drain_races();
        // Goroutine 112 reads, goroutine 113 writes with no
        // synchronisation between them: write-after-read race.
        record_access(112, 0xBBBB, false);
        record_access(113, 0xBBBB, true);
        let races = drain_races();
        assert!(!races.is_empty(), "expected WAR race, got {races:?}");
        assert!(
            races
                .iter()
                .any(|r| r.contains("reader=112") && r.contains("writer=113")),
            "race message should name reader and writer: {races:?}",
        );
    }

    #[test]
    fn detector_does_not_flag_synchronised_writes() {
        let _g = TEST_GUARD.lock();
        enable();
        let _ = drain_races();
        // Goroutine 103 writes, hands off via record_sync to 104
        // which also writes — 103's write happens-before 104's.
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
    fn detector_does_not_flag_synchronised_read_after_write() {
        let _g = TEST_GUARD.lock();
        enable();
        let _ = drain_races();
        // Writer syncs to reader via record_sync — no race.
        record_access(120, 0xCCCC, true);
        record_sync(120, 121);
        record_access(121, 0xCCCC, false);
        let races = drain_races();
        assert!(
            races.is_empty(),
            "synchronised RAW flagged as race: {races:?}"
        );
    }

    #[test]
    fn detector_tracks_multiple_concurrent_readers_for_war() {
        let _g = TEST_GUARD.lock();
        enable();
        let _ = drain_races();
        // Three goroutines read, then a fourth writes without sync.
        // All three read-write pairs should be flagged.
        record_access(130, 0xDDDD, false);
        record_access(131, 0xDDDD, false);
        record_access(132, 0xDDDD, false);
        record_access(133, 0xDDDD, true);
        let races = drain_races();
        assert!(races.len() >= 3, "expected 3 WAR races, got: {races:?}");
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
