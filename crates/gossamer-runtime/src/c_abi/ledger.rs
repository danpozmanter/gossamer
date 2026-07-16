//! Allocation ledger: per-family live-object counters, printed at process exit
//! when `GOS_LEAK_LEDGER` is set. Deterministic leak detection - a family whose
//! live count grows with the workload size N (instead of staying O(1)) is
//! leaking. Used to lock leak targets and prove fixes (see
//! `~/dev/contexts/gos/leaks.md`).
//!
//! The counters are `Relaxed` atomics (cheap); the at-exit hook is armed once.

use std::sync::{
    LazyLock,
    atomic::{AtomicI64, AtomicU64, Ordering},
};

/// Runtime-managed work observed during one benchmark measurement scope.
///
/// The counters are deliberately thread-local: benchmark targets may run in
/// parallel, and each target must report only the allocations and ownership
/// work performed by its own VM. They cover runtime allocations, reference
/// count operations, and bytes copied by the VM/JIT trampoline, rather than
/// arbitrary Rust allocator activity in the compiler or harness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BenchmarkCounters {
    /// Runtime-managed allocations, including RC nodes, strings, and Vec storage.
    pub allocations: u64,
    /// Requested bytes for the counted runtime-managed allocations.
    pub allocation_bytes: u64,
    /// Runtime ARC retain operations.
    pub arc_retains: u64,
    /// Runtime ARC release operations.
    pub arc_releases: u64,
    /// VM/JIT aggregate or string marshalling copies.
    pub boundary_copies: u64,
    /// Bytes transferred by VM/JIT marshalling copies.
    pub boundary_copy_bytes: u64,
}

thread_local! {
    static BENCHMARK_COUNTERS: std::cell::Cell<(bool, BenchmarkCounters)> =
        const { std::cell::Cell::new((false, BenchmarkCounters {
            allocations: 0,
            allocation_bytes: 0,
            arc_retains: 0,
            arc_releases: 0,
            boundary_copies: 0,
            boundary_copy_bytes: 0,
        })) };
}

/// Start a fresh per-thread benchmark measurement scope.
pub fn begin_benchmark_counters() {
    BENCHMARK_COUNTERS.with(|counters| counters.set((true, BenchmarkCounters::default())));
}

/// Stop the current per-thread benchmark measurement scope and return its data.
#[must_use]
pub fn finish_benchmark_counters() -> BenchmarkCounters {
    BENCHMARK_COUNTERS.with(|counters| {
        let (enabled, snapshot) = counters.get();
        counters.set((false, BenchmarkCounters::default()));
        if enabled {
            snapshot
        } else {
            BenchmarkCounters::default()
        }
    })
}

#[inline]
fn with_benchmark_counters(update: impl FnOnce(&mut BenchmarkCounters)) {
    BENCHMARK_COUNTERS.with(|counters| {
        let (enabled, mut snapshot) = counters.get();
        if enabled {
            update(&mut snapshot);
            counters.set((enabled, snapshot));
        }
    });
}

/// Record one runtime-managed allocation and its requested byte footprint.
#[inline]
pub fn benchmark_allocation(bytes: usize) {
    with_benchmark_counters(|counters| {
        counters.allocations = counters.allocations.saturating_add(1);
        counters.allocation_bytes = counters.allocation_bytes.saturating_add(bytes as u64);
    });
}

/// Record one runtime ARC retain.
#[inline]
pub fn benchmark_arc_retain() {
    with_benchmark_counters(|counters| {
        counters.arc_retains = counters.arc_retains.saturating_add(1);
    });
}

/// Record one runtime ARC release.
#[inline]
pub fn benchmark_arc_release() {
    with_benchmark_counters(|counters| {
        counters.arc_releases = counters.arc_releases.saturating_add(1);
    });
}

/// Record one VM/JIT boundary copy and the bytes moved.
#[inline]
pub fn benchmark_boundary_copy(bytes: usize) {
    with_benchmark_counters(|counters| {
        counters.boundary_copies = counters.boundary_copies.saturating_add(1);
        counters.boundary_copy_bytes = counters.boundary_copy_bytes.saturating_add(bytes as u64);
    });
}

pub static AGGR_LIVE: AtomicI64 = AtomicI64::new(0);
pub static RC_LIVE: AtomicI64 = AtomicI64::new(0);
pub static STR_LIVE: AtomicI64 = AtomicI64::new(0);
pub static VEC_LIVE: AtomicI64 = AtomicI64::new(0);
pub static MAP_LIVE: AtomicI64 = AtomicI64::new(0);

// Allocation-shape counters for the compact GosVec layout. These are totals
// rather than live counts: the point is to expose how often a workload pays
// for the header, owner carrier, inline payload, or a split buffer. They are
// reported only when `GOS_VEC_ALLOC_STATS=1` is set.
pub static VEC_INLINE_ALLOCS: AtomicU64 = AtomicU64::new(0);
pub static VEC_SPLIT_ALLOCS: AtomicU64 = AtomicU64::new(0);
pub static VEC_OWNER_ALLOCS: AtomicU64 = AtomicU64::new(0);
pub static VEC_REGION_ALLOCS: AtomicU64 = AtomicU64::new(0);
pub static VEC_REQUESTED_BYTES: AtomicU64 = AtomicU64::new(0);
pub static VEC_USABLE_BYTES: AtomicU64 = AtomicU64::new(0);

// RC allocation-shape counters.  `payload_bytes` deliberately excludes the
// fixed eight-byte RcHeader, while `usable_bytes` is the allocator capacity
// for the complete block.  Keeping both makes header and allocator-bin costs
// visible for recursive-enum workloads without changing the header ABI.
// Region allocations are reported separately because they are suballocated
// from an arena slab and therefore have no meaningful per-object usable size.
pub static RC_HEAP_ALLOCS: AtomicU64 = AtomicU64::new(0);
pub static RC_REGION_ALLOCS: AtomicU64 = AtomicU64::new(0);
pub static RC_REUSE_ALLOCS: AtomicU64 = AtomicU64::new(0);
pub static RC_PAYLOAD_BYTES: AtomicU64 = AtomicU64::new(0);
pub static RC_USABLE_BYTES: AtomicU64 = AtomicU64::new(0);

// String-key map workload counters. These distinguish borrowed probes from
// the unavoidable owned-key allocation on a first insertion, and make
// formatting's temporary work visible without changing map semantics.
pub static MAP_STR_PROBES: AtomicU64 = AtomicU64::new(0);
pub static MAP_STR_KEY_COPIES: AtomicU64 = AtomicU64::new(0);
pub static MAP_STR_KEY_COPY_BYTES: AtomicU64 = AtomicU64::new(0);
pub static MAP_FORMAT_CALLS: AtomicU64 = AtomicU64::new(0);
pub static MAP_FORMAT_ENTRIES: AtomicU64 = AtomicU64::new(0);

static VEC_ALLOC_STATS_ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("GOS_VEC_ALLOC_STATS").is_some());
static RC_ALLOC_STATS_ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("GOS_RC_ALLOC_STATS").is_some());
static MAP_ALLOC_STATS_ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("GOS_MAP_ALLOC_STATS").is_some());

#[inline]
fn vec_alloc_stats_enabled() -> bool {
    cfg!(test) || *VEC_ALLOC_STATS_ENABLED
}

#[inline]
fn rc_alloc_stats_enabled() -> bool {
    cfg!(test) || *RC_ALLOC_STATS_ENABLED
}

#[inline]
fn map_alloc_stats_enabled() -> bool {
    cfg!(test) || *MAP_ALLOC_STATS_ENABLED
}

#[cfg(all(unix, not(miri)))]
static ARMED: std::sync::Once = std::sync::Once::new();

#[cfg(all(unix, not(miri)))]
extern "C" fn report() {
    if std::env::var("GOS_LEAK_LEDGER").is_ok() {
        eprintln!(
            "LEAK LEDGER (live at exit): aggr={} rc={} str={} vec={} map={}",
            AGGR_LIVE.load(Ordering::SeqCst),
            RC_LIVE.load(Ordering::SeqCst),
            STR_LIVE.load(Ordering::SeqCst),
            VEC_LIVE.load(Ordering::SeqCst),
            MAP_LIVE.load(Ordering::SeqCst),
        );
    }
    if std::env::var("GOS_VEC_ALLOC_STATS").is_ok() {
        eprintln!(
            "VEC ALLOC STATS: inline={} split={} owner={} region={} requested_bytes={} usable_bytes={}",
            VEC_INLINE_ALLOCS.load(Ordering::Relaxed),
            VEC_SPLIT_ALLOCS.load(Ordering::Relaxed),
            VEC_OWNER_ALLOCS.load(Ordering::Relaxed),
            VEC_REGION_ALLOCS.load(Ordering::Relaxed),
            VEC_REQUESTED_BYTES.load(Ordering::Relaxed),
            VEC_USABLE_BYTES.load(Ordering::Relaxed),
        );
    }
    if std::env::var("GOS_RC_ALLOC_STATS").is_ok() {
        eprintln!(
            "RC ALLOC STATS: heap={} region={} reuse={} payload_bytes={} usable_bytes={}",
            RC_HEAP_ALLOCS.load(Ordering::Relaxed),
            RC_REGION_ALLOCS.load(Ordering::Relaxed),
            RC_REUSE_ALLOCS.load(Ordering::Relaxed),
            RC_PAYLOAD_BYTES.load(Ordering::Relaxed),
            RC_USABLE_BYTES.load(Ordering::Relaxed),
        );
    }
    if std::env::var("GOS_MAP_ALLOC_STATS").is_ok() {
        eprintln!(
            "MAP STRING STATS: probes={} key_copies={} key_copy_bytes={} format_calls={} format_entries={}",
            MAP_STR_PROBES.load(Ordering::Relaxed),
            MAP_STR_KEY_COPIES.load(Ordering::Relaxed),
            MAP_STR_KEY_COPY_BYTES.load(Ordering::Relaxed),
            MAP_FORMAT_CALLS.load(Ordering::Relaxed),
            MAP_FORMAT_ENTRIES.load(Ordering::Relaxed),
        );
    }
}

// At-exit auto-print of the ledger is unix-only (`libc::atexit`) and is
// additionally skipped under Miri, which cannot execute `atexit` (and runs
// with `-Zmiri-ignore-leaks`, so the report is moot there anyway). On other
// targets the counters still tally but the report is read via a debugger /
// explicit query rather than printed at exit. This keeps the Windows build -
// where `libc` is not a dependency - and the Miri job compiling and running.
#[inline]
fn arm() {
    #[cfg(all(unix, not(miri)))]
    ARMED.call_once(|| unsafe {
        libc::atexit(report);
    });
}

#[inline]
pub fn aggr_inc() {
    arm();
    AGGR_LIVE.fetch_add(1, Ordering::Relaxed);
}
#[inline]
pub fn aggr_dec() {
    AGGR_LIVE.fetch_sub(1, Ordering::Relaxed);
}
#[inline]
pub fn rc_inc() {
    arm();
    RC_LIVE.fetch_add(1, Ordering::Relaxed);
}
#[inline]
pub fn rc_dec() {
    RC_LIVE.fetch_sub(1, Ordering::Relaxed);
}
#[inline]
pub fn str_inc() {
    arm();
    STR_LIVE.fetch_add(1, Ordering::Relaxed);
}
#[inline]
pub fn str_dec() {
    STR_LIVE.fetch_sub(1, Ordering::Relaxed);
}
#[inline]
pub fn vec_inc() {
    arm();
    VEC_LIVE.fetch_add(1, Ordering::Relaxed);
}
#[inline]
pub fn vec_dec() {
    VEC_LIVE.fetch_sub(1, Ordering::Relaxed);
}

#[inline]
fn record_vec_bytes(requested: usize, usable: usize) {
    VEC_REQUESTED_BYTES.fetch_add(requested as u64, Ordering::Relaxed);
    VEC_USABLE_BYTES.fetch_add(usable as u64, Ordering::Relaxed);
}

/// Record one compact Vec allocation. `usable` is queried from mimalloc when
/// it owns the allocation; sanitizer/Miri/wasm builds report the requested
/// layout size instead because they intentionally use a different allocator.
#[inline]
pub fn vec_inline_alloc(requested: usize, usable: usize) {
    benchmark_allocation(requested);
    if !vec_alloc_stats_enabled() {
        return;
    }
    arm();
    VEC_INLINE_ALLOCS.fetch_add(1, Ordering::Relaxed);
    record_vec_bytes(requested, usable);
}

#[inline]
pub fn vec_split_alloc(requested: usize, usable: usize) {
    benchmark_allocation(requested);
    if !vec_alloc_stats_enabled() {
        return;
    }
    arm();
    VEC_SPLIT_ALLOCS.fetch_add(1, Ordering::Relaxed);
    record_vec_bytes(requested, usable);
}

#[inline]
pub fn vec_owner_alloc(requested: usize, usable: usize) {
    benchmark_allocation(requested);
    if !vec_alloc_stats_enabled() {
        return;
    }
    arm();
    VEC_OWNER_ALLOCS.fetch_add(1, Ordering::Relaxed);
    record_vec_bytes(requested, usable);
}

#[inline]
pub fn vec_region_alloc(requested: usize) {
    benchmark_allocation(requested);
    if !vec_alloc_stats_enabled() {
        return;
    }
    arm();
    VEC_REGION_ALLOCS.fetch_add(1, Ordering::Relaxed);
    // Region slabs are suballocated. Attribute their requested payload but do
    // not pretend the slab's allocator capacity belongs to one Vec.
    record_vec_bytes(requested, requested);
}

/// Record an RC allocation's user payload and allocator footprint.
///
/// `usable` includes the compact eight-byte RC header. For a region object it
/// is exactly the requested block size because the object is suballocated from
/// a slab rather than owned by the system allocator. A Perceus reuse records
/// the existing block's usable size and does not increase `heap`.
#[inline]
pub fn rc_alloc(payload: usize, usable: usize, region: bool, reuse: bool) {
    benchmark_allocation(payload);
    if !rc_alloc_stats_enabled() {
        return;
    }
    arm();
    if region {
        RC_REGION_ALLOCS.fetch_add(1, Ordering::Relaxed);
    } else if reuse {
        RC_REUSE_ALLOCS.fetch_add(1, Ordering::Relaxed);
    } else {
        RC_HEAP_ALLOCS.fetch_add(1, Ordering::Relaxed);
    }
    RC_PAYLOAD_BYTES.fetch_add(payload as u64, Ordering::Relaxed);
    RC_USABLE_BYTES.fetch_add(usable as u64, Ordering::Relaxed);
}

/// Snapshot the RC allocation-shape counters for focused runtime tests and
/// embedders that collect their own benchmark records.
#[must_use]
pub fn rc_alloc_stats() -> (u64, u64, u64, u64, u64) {
    (
        RC_HEAP_ALLOCS.load(Ordering::Relaxed),
        RC_REGION_ALLOCS.load(Ordering::Relaxed),
        RC_REUSE_ALLOCS.load(Ordering::Relaxed),
        RC_PAYLOAD_BYTES.load(Ordering::Relaxed),
        RC_USABLE_BYTES.load(Ordering::Relaxed),
    )
}
#[inline]
pub fn map_inc() {
    arm();
    MAP_LIVE.fetch_add(1, Ordering::Relaxed);
}
#[inline]
pub fn map_dec() {
    MAP_LIVE.fetch_sub(1, Ordering::Relaxed);
}

#[inline]
pub fn map_str_probe() {
    if !map_alloc_stats_enabled() {
        return;
    }
    arm();
    MAP_STR_PROBES.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn map_str_key_copy(bytes: usize) {
    if !map_alloc_stats_enabled() {
        return;
    }
    arm();
    MAP_STR_KEY_COPIES.fetch_add(1, Ordering::Relaxed);
    MAP_STR_KEY_COPY_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
}

#[inline]
pub fn map_format(entries: usize) {
    if !map_alloc_stats_enabled() {
        return;
    }
    arm();
    MAP_FORMAT_CALLS.fetch_add(1, Ordering::Relaxed);
    MAP_FORMAT_ENTRIES.fetch_add(entries as u64, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_counters_are_scoped_and_thread_local() {
        begin_benchmark_counters();
        benchmark_allocation(24);
        benchmark_arc_retain();
        benchmark_arc_release();
        benchmark_boundary_copy(7);
        assert_eq!(
            finish_benchmark_counters(),
            BenchmarkCounters {
                allocations: 1,
                allocation_bytes: 24,
                arc_retains: 1,
                arc_releases: 1,
                boundary_copies: 1,
                boundary_copy_bytes: 7,
            }
        );
        assert_eq!(finish_benchmark_counters(), BenchmarkCounters::default());

        let other = std::thread::spawn(|| {
            begin_benchmark_counters();
            benchmark_allocation(3);
            finish_benchmark_counters()
        })
        .join()
        .expect("benchmark counter thread must finish");
        assert_eq!(other.allocations, 1);
        assert_eq!(other.allocation_bytes, 3);
        assert_eq!(finish_benchmark_counters(), BenchmarkCounters::default());
    }
}
