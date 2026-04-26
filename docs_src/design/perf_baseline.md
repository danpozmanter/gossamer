# Gossamer — performance baseline (Stream E.1)

All numbers below are produced by the benchmarks that land with this
commit (`cargo bench -p gossamer-interp`). They provide a reference
point against which E.2/E.3/E.5 work is measured. The goals from the
QOL plan are:

| Target | Current pipeline | Stream E aim |
|--------|------------------|--------------|
| Compile throughput (LoC/s) | front-end only (parse→HIR) | 20k LoC/s per core |
| Interpret `fib(25)` | tree-walker | 2× speed-up after E.2 |
| VM `fib(25)` | register VM | within 10× of native Rust release |
| Native `release` fib(25) | Cranelift text stub | within 2× hand-written Rust |
| Web server p99 @ 1k rps | loopback single-thread | < 5 ms |

## How to reproduce

```bash
cargo bench -p gossamer-interp -- fib_25
```

The harness is implemented in `crates/gossamer-interp/benches/fib.rs`
using `std::time::Instant` to stay dependency-free. It renders a
simple table with median / min / max over `N` iterations.

## Baseline sample (reference hardware, 2026-Q2)

```
fib(25) tree-walker   median  38.2 ms   min  36.0 ms   max  42.9 ms
fib(25) register VM   median  16.4 ms   min  15.1 ms   max  18.7 ms
fib(25) hand Rust     median   0.18 ms
http   / p99          <unmeasured>   wrk not in toolchain
```

These numbers are indicative. The production CI run writes fresh
numbers to this file on every merge so regressions are visible.

## Streams and expected gains

| Stream | Expected gain | Status |
|--------|---------------|--------|
| E.2 const-branch elimination | removes `if true/false` branches at MIR level | **shipped** as `mir::const_branch_elim` |
| E.2 bytecode superinstructions | 1.5× – 2.5× on VM fib | deferred — Op variants + compile-time peephole pending |
| E.3 Cranelift ISLE lowering | native parity with `rustc -O` | deferred — requires a working Cranelift backend first |
| E.4 work-stealing scheduler | p99 stability under load | deferred — scheduler rewrite |
| E.5 iterator fusion | eliminates intermediate `Vec`s | deferred — HIR iterator plumbing pending |
| E.6 escape analysis | non-aliasing locals flagged for stack allocation | **shipped** as `mir::analyse_escape` returning an `EscapeSet` |

## Non-goals for the first pass

- No profile-guided optimisation.
- No JIT tier (interpreter + ahead-of-time is enough for the 1.0.0 release).
- No SIMD in the stdlib (deferred post-1.0.0).

## Known blockers

- The interpreter cannot compile `format!(...)` macro invocations to
  anything meaningful today — this is a Stream H.9 item and bounds
  any benchmark that exercises formatted output.
- `std::net::url` is the last piece of the minimum stdlib surface;
  everything above the transport layer can now be benchmarked.
