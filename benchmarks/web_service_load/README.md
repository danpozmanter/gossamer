# web_service_load

Joint Track A + Track B load validation. Runs the
`examples/projects/web_service_full/` HTTPS service against a
synthetic load generator, asserts:

1. **OS thread count** stays bounded by `GOMAXPROCS` (≈ `nproc`)
   - proves the netpoller is doing its job and we are not a
   thread-per-connection server.
2. **No goroutine leak** across a 30-minute soak - proves
   parked goroutines on I/O wake correctly when the kernel
   reports readiness.
3. **Resident memory stays bounded** under continuous
   allocation pressure - proves deterministic reference counting
   reclaims request-scoped allocations promptly, with no
   unbounded growth and no collector pauses (there is no
   tracing GC).

If any assertion fails, file the regression back to Track A - this
is the proof their work composes with Track B's HTTP/sqlite stack.

## Quickstart

```sh
./run.sh                     # uses bundled wrk-style harness
./run.sh --connections=10000 # increase fan-out
./run.sh --soak              # 30-minute soak (CI gate)
./run.sh --metrics           # print final MemStats + goroutine count
```

The bundled harness (`harness.gos`) opens N concurrent HTTPS
clients in goroutines, each issuing `GET /notes` + `POST /notes`
in a steady loop. Metrics are scraped from the service's
`/debug/pprof/heap`, `/debug/pprof/goroutine`, and a custom
`/debug/metrics` endpoint added by the example service.

## Why a bundled harness, not vegeta / wrk

Vegeta (Go) and wrk (C) are great but they pin extra runtime
deps; CI runners that need to install them on every job spend
more time installing than testing. The bundled harness is a
Gossamer program - same toolchain - so the soak is one
`gos benchmarks/web_service_load/harness.gos` away. The
`./run.sh --vegeta` path is wired for users who already have
vegeta and want richer percentile output.

## Assertions (CI gates)

The run script fails non-zero if:

- Number of OS threads in the service process exceeds
  `GOMAXPROCS * 2 + 4` (allowing for poller / signal-relay
  threads).
- Goroutine count after the soak is more than `1.5x` the
  steady-state midpoint.
- Resident memory after the soak keeps growing instead of
  tracking the steady-state working set (deterministic
  reclamation should keep RSS flat).

Pass thresholds are conservative; tighten them as Track A's
runtime matures.
