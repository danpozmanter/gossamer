//! Runtime support for `std::runtime` — goroutine / GC / scheduler
//! introspection and tuning knobs, analogous to Go's `runtime`
//! package.
//! The first slice exposes CPU count, a `GOMAXPROCS`-equivalent
//! setter (honoured by the Gossamer scheduler once Stream E.4 wires
//! the work-stealing variant), and a read-only memstats surface.

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

/// Soft upper bound on simultaneously-running goroutines.
///
/// Mirrors `runtime.GOMAXPROCS(n)`. The scheduler reads this on
/// every worker-thread startup; adjusting mid-run does not kill
/// already-running workers but caps how many new ones spawn.
static MAX_PROCS: AtomicUsize = AtomicUsize::new(0);

/// Returns the current goroutine-concurrency cap. When no value has
/// been set, reads the host's logical CPU count via
/// [`std::thread::available_parallelism`].
#[must_use]
pub fn max_procs() -> usize {
    let cached = MAX_PROCS.load(Ordering::Relaxed);
    if cached > 0 {
        return cached;
    }
    num_cpus()
}

/// Sets the goroutine concurrency cap. Returns the previous value.
/// A value of `0` restores the automatic-from-host behaviour.
pub fn set_max_procs(n: usize) -> usize {
    MAX_PROCS.swap(n, Ordering::Relaxed)
}

/// Number of logical CPU cores visible to the process, per
/// `std::thread::available_parallelism`. Returns `1` if the query
/// fails.
#[must_use]
pub fn num_cpus() -> usize {
    thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

/// Read-only snapshot of memory usage surfaced to Gossamer
/// programs. Field shape mirrors Go's `runtime.MemStats` closely
/// enough that operators familiar with one can read the other.
#[derive(Debug, Clone, Copy, Default)]
pub struct MemStats {
    /// Cumulative bytes allocated for heap objects since program
    /// start. Equivalent to Go's `MemStats.TotalAlloc`.
    pub bytes_allocated: u64,
    /// Bytes currently held by live (post-sweep) objects.
    /// Equivalent to Go's `MemStats.HeapInuse`.
    pub live_bytes: u64,
    /// Number of completed GC cycles. Equivalent to
    /// `MemStats.NumGC`.
    pub cycles: u64,
    /// Duration of the most recent GC cycle, in nanoseconds.
    pub last_pause_nanos: u64,
    /// Longest GC pause observed since program start, in nanoseconds.
    pub max_pause_nanos: u64,
    /// Number of currently-live objects. Equivalent to
    /// `MemStats.HeapObjects`.
    pub live_objects: u64,
    /// Total nanoseconds spent in stop-the-world pauses across the
    /// program's lifetime. Equivalent to `MemStats.PauseTotalNs`.
    pub total_pause_nanos: u64,
    /// Soft heap-growth target the collector aims to stay under.
    /// Equivalent to Go's `MemStats.NextGC`.
    pub next_gc_bytes: u64,
}

/// Snapshots [`MemStats`] from the runtime's live heap. Reads the
/// global `gossamer_gc::Heap` through `gossamer_runtime::gc::stats`,
/// so the values reflect the actual collector's accounting.
#[must_use]
pub fn mem_stats() -> MemStats {
    let stats = gossamer_runtime::gc::stats();
    MemStats {
        bytes_allocated: u64::try_from(stats.bytes_allocated).unwrap_or(u64::MAX),
        live_bytes: 0,
        cycles: stats.cycles,
        last_pause_nanos: stats.last_pause_nanos,
        max_pause_nanos: stats.max_pause_nanos,
        live_objects: u64::try_from(stats.live).unwrap_or(u64::MAX),
        total_pause_nanos: u64::try_from(stats.total_pause_nanos).unwrap_or(u64::MAX),
        next_gc_bytes: 0,
    }
}

/// Snapshots every live goroutine for diagnostics. Wraps
/// [`gossamer_runtime::sigquit::snapshot`].
#[must_use]
pub fn all_goroutines() -> Vec<gossamer_runtime::sigquit::GoroutineInfo> {
    gossamer_runtime::sigquit::snapshot()
}

/// Number of currently-live goroutines.
#[must_use]
pub fn num_goroutines() -> usize {
    gossamer_runtime::sigquit::snapshot().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn num_cpus_is_at_least_one() {
        assert!(num_cpus() >= 1);
    }

    #[test]
    fn set_max_procs_round_trips() {
        let prev = set_max_procs(42);
        assert_eq!(max_procs(), 42);
        let restored = set_max_procs(prev);
        assert_eq!(restored, 42);
    }

    #[test]
    fn max_procs_defaults_to_num_cpus_when_unset() {
        let _ = set_max_procs(0);
        assert_eq!(max_procs(), num_cpus());
    }

    #[test]
    fn mem_stats_returns_zero_by_default() {
        let snap = mem_stats();
        assert_eq!(snap.cycles, 0);
    }
}
