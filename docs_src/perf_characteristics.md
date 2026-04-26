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
2. **Spawn cost** ≈ 30–80 µs per `go expr` (kernel
   `clone` + Gossamer scheduler bookkeeping). Compared to Go's
   ~2 µs spawn this is slow by two orders of magnitude.
3. **Context switches** are kernel-driven. The OS scheduler is
   in charge; Gossamer does not park goroutines in user space.

This is the *Go-1-style* implementation, not the modern Go
scheduler. It is correct, simple, and sufficient for service
loads up to a few thousand concurrent goroutines. **M:N
scheduling lands in v1.x** and brings goroutine cost down to
Go-class.

For workloads that today need 10k+ goroutines (Go's bread-and-butter
"a goroutine per connection" pattern), v1 is workable but not
ideal — limit goroutines to a worker pool sized at 2–4× the
core count.

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

## Comparison to Go's runtime

| Metric | Go (current) | Gossamer (v1) | Notes |
|---|---|---|---|
| Goroutine spawn | ~2 µs | ~30–80 µs | M:N pending in v1.x. |
| Goroutine memory | ~8 KiB (growable stacks) | ~1 MiB (OS-thread stack) | Same as above. |
| GC pause (live=200 MB) | <1 ms (concurrent) | 3–8 ms (STW) | Concurrent GC pending. |
| `chan` send | ~50 ns | ~150 ns | Mutex-based; lock-free queue pending. |
| HTTP throughput (echo) | ~80k req/s/core | ~30k req/s/core | M:N + chan tuning will close this. |

These are not promises — they are the targets the runtime is
sized against. Your milage will vary; please publish numbers
back if they diverge.

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
