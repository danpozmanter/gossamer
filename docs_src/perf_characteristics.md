# Performance characteristics

This page is the user-facing companion to the design doc
[`design/perf_baseline.md`](design/perf_baseline.md). It covers
what to expect from Gossamer's runtime in production: garbage
collection behaviour, per-goroutine memory, scheduling under
load, and how the compiled tier compares to Go and Rust.

The numbers below are reproducible on commodity hardware; the
runs in this document are from an AMD Ryzen 9 9900X (12 cores
/ 24 threads, 64 GiB DDR5-6000) on Linux 6.17. Your numbers
will differ; the *shape* of the curves is what matters.

## Compiled-tier benchmarks

| benchmark | mode | secs | cpu secs | mem (kb) | gz (bytes) |
|-----------|------|-----:|---------:|---------:|-----------:|
| fasta (N=25M) | Rust reference | 0.84 | 4.32 | 8 660 | 1 933 |
| fasta (N=25M) | Go reference | 0.63 | 2.56 | 17 956 | 1 827 |
| fasta (N=25M) | Gossamer `--release` | **0.49** | 2.02 | 1 564 288 | 2 297 |
| n-body (N=50M) | Rust reference | 1.46 | 1.46 | 1 952 | 2 033 |
| n-body (N=50M) | Go reference | 2.13 | 2.13 | 2 204 | 1 215 |
| n-body (N=50M) | Gossamer `--release` (opt) | 1.69 | 1.68 | 2 120 | 2 628 |

Reproduce with `bash bench/game/run.sh` against any commit. The
multi-thread fasta uses 8 worker goroutines + a shared `I64Vec`;
the n-body opt variant uses precomputed pair distances to match
the Rust reference's algorithm shape.

The fasta memory cost (~1.5 GiB) is the price of the
`I64Vec`-backed parallel buffer: 8 bytes per output character
× 50M characters in flight ≈ 400 MB per section, doubled across
the two random sections. The single-thread variant
(`fasta_old.gos`) runs at ~1.8 s using ~2 MiB.

## Garbage collector

Gossamer ships a stop-the-world mark-sweep GC. Expect:

- **Pause time** scales with live-set size, not heap size.
  Steady-state pauses on a 200 MiB live set: ~3–8 ms in the
  `mem_stats()` histogram. Spikes when many large allocations
  retire at once.
- **Heap growth** target is 1.5× live (configurable via
  `GOSSAMER_GC_TARGET`). Higher = fewer pauses, more RAM.
- **Allocation rate** for typical service code: 200–500 MB/s
  per thread. Closures, `String::new`, and tuple-return idioms
  are the hot allocators; consider `Vec::with_capacity`-style
  preallocation in inner loops.
- **Memory not returned to OS** aggressively. The runtime
  reuses freed pages; OS-visible RSS only shrinks under
  prolonged idle. Container memory limits should be sized to
  ~2× peak working set.

Tail-latency-sensitive services (p99 < 10 ms) should:

- Pre-allocate Vec / HashMap with `with_capacity`.
- Avoid per-request closures; reuse handlers.
- Consider pooling large buffers via `bytes::Buffer`.

## Goroutines

In v1, every goroutine is a real OS thread. This has three
consequences:

1. **Memory cost per goroutine** ≈ 1 MiB stack (Linux default,
   reducible via `ulimit -s`) + ~32 KiB runtime metadata.
   Spawning 10 000 goroutines costs ~10 GiB of RSS unless you
   shrink stacks.
2. **Spawn cost** ≈ a few µs per `go expr` since the
   work-stealing scheduler landed in 0.1.0 — goroutines no
   longer ride a 1:1 OS-thread mapping. The cost on a warm
   process is dominated by the deque push and a wake-one
   notify on a parked worker.
3. **Context switches** are user-space within the scheduler:
   `Mutex` / `Channel` / `WaitGroup` park goroutines on the
   `MultiScheduler` rather than the OS thread, so worker
   threads stay free even under contention.
4. **I/O is netpoller-driven**: a goroutine blocked on
   `tcp::read` does not occupy a worker thread. The
   `epoll` / `kqueue` / `IOCP` backend wakes the goroutine
   when readiness arrives.

This brings the runtime to Go-class on the I/O-bound path:
10k+ idle TCP connections held by `~GOMAXPROCS` OS threads
is the steady-state shape, validated by
`benchmarks/web_service_load/`.

## Throughput vs. latency

The runtime favours throughput:

- 64 KiB stdout buffer with line-buffer flush. Programs that
  print 25 MB in tight inner loops (fasta) reach the kernel via
  ~400 syscalls instead of 25 million.
- Inline-cached method dispatch in the bytecode VM saves the
  `HashMap::get` lookup on hot paths.
- LLVM `-O3 -mcpu=native` per-function with auto-vectorisation
  enabled. Inner loops match the `rustc -C target-cpu=native`
  output for primitive arithmetic.
- Per-function fallback to Cranelift means a partially-LLVM
  program still compiles. Cranelift's output is roughly 1.5–2×
  slower than LLVM for numeric kernels.

Latency tuning options:

- `GOSSAMER_GC_TARGET=2.0` — fewer collections, higher peak RSS.
- `GOSSAMER_PROCS=N` — pin scheduler to N OS threads.
- Pre-warm caches on startup; the first request after launch
  pays JIT + GC initialisation overhead.

## Scheduling under load

Sustained load behaviour, measured against `bench/game/fasta`
running 8 fan-out workers at N=25M:

- All 8 OS threads pegged at ≥99% utilisation throughout. No
  scheduler-induced idle.
- Output ordering deterministic: each worker fills a non-overlapping
  range of the shared `I64Vec`, then `WaitGroup::wait` joins
  before the bulk write.
- No global mutex contention in the steady state. The GC's
  STW phase is the only synchronisation point.

For HTTP services the picture is different: each request runs on
its own goroutine, and idle goroutines park on socket reads. RSS
grows linearly with concurrent connection count due to the
1 MiB-per-goroutine stack.

## Comparison to Go's runtime (0.1.0 baseline)

| Metric | Go (1.22) | Gossamer (0.1.0) | Notes |
|---|---|---|---|
| Goroutine spawn | ~2 µs | ~3–6 µs | Crossbeam-deque + park/unpark. |
| Goroutine memory | ~8 KiB (growable stacks) | ~8–32 KiB (Box+state) | No growable user stacks; cooperative model. |
| GC pause (live=200 MB) | <1 ms (concurrent) | 1–4 ms (concurrent + STW finish) | Write barrier emitted; final remark STW. |
| `chan` send | ~50 ns | ~80–150 ns | parking_lot-backed bounded channel. |
| HTTP throughput (echo) | ~80k req/s/core | ~50–70k req/s/core | Netpoller live; ureq-backed client. |
| 10k idle TCP conns | `~GOMAXPROCS` threads | `~GOMAXPROCS` threads | Both rely on netpoller park. |

Targets, not promises — the
[benchmarks/web_service_load](https://github.com/danpozmanter/gossamer/tree/main/benchmarks/web_service_load)
harness is the ongoing measurement. CI tracks
the steady-state number per release.

## Comparison to Rust's runtime (no async)

For CPU-bound benchmarks, Gossamer `--release` is competitive
with `rustc -C opt-level=3` because:

- Both lower to LLVM IR with `-O3 -mcpu=native`.
- Both avoid heap allocation in inner loops.
- Gossamer's runtime is lighter (no Rust standard-library
  threading machinery).

For allocation-heavy code, Rust pulls ahead because Rust has no
GC overhead. Code that builds many small `String`s in a loop
will look better in Rust unless the Gossamer code preallocates.

## Cross-references

- [`design/perf_baseline.md`](design/perf_baseline.md) — full
  baseline numbers and method.
- [`design/binary_size_baseline.md`](design/binary_size_baseline.md) —
  output binary sizes.
- [`deployment.md`](deployment.md) — production tuning knobs.
- [`non_goals_v1.md`](non_goals_v1.md) — what perf gaps are
  intentional in v1.
