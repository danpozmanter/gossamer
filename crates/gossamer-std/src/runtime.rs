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
/// programs. Populated from `gossamer-gc::GcStats` + host memory
/// accounting as the GC matures.
#[derive(Debug, Clone, Copy, Default)]
pub struct MemStats {
    /// Total bytes allocated since program start.
    pub bytes_allocated: u64,
    /// Live bytes currently on the GC heap (best-effort).
    pub live_bytes: u64,
    /// Number of completed GC cycles.
    pub cycles: u64,
    /// Duration of the most recent GC cycle, in nanoseconds.
    pub last_pause_nanos: u64,
    /// Longest GC pause so far, in nanoseconds.
    pub max_pause_nanos: u64,
}

/// Returns a zero-filled [`MemStats`] today — will pull from the
/// live `Heap` once the interpreter exposes one through this module
/// (tracked under gaps.md §2.5).
#[must_use]
pub fn mem_stats() -> MemStats {
    MemStats::default()
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
