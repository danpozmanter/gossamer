# Debugging guide

Four production-incident workflows the runtime supports today.

## 1. Attaching `gdb` / `lldb`

Build with `--release -g` to embed DWARF:

```sh
gos build --release -g src/main.gos
```

Then:

```sh
gdb ./main
(gdb) break main
(gdb) run
(gdb) backtrace
```

Or `lldb`:

```sh
lldb ./main -- arg1 arg2
(lldb) breakpoint set --name main
(lldb) run
```

The `-g` flag emits `DICompileUnit` + `DISubprogram` per
function, with `!dbg` attached to each `define`. Source-line
columns within a function are best-effort today (the
SourceMap-driven per-instruction map is a Phase-2 follow-up);
function-name granularity works.

## 2. Reading a `SIGQUIT` dump

Sending `SIGQUIT` to a Gossamer process (or pressing Ctrl-\\
on a foreground process) prints every live goroutine to
stderr and exits non-zero. Output format:

```text
SIGQUIT: dumping 1342 goroutine(s)

goroutine 17 [running]:
  main::handle_request()
        src/main.gos:128
        0:   <host backtrace frame>
        1:   <host backtrace frame>
        ...

goroutine 18 [chan receive]:
  ...
```

The header line shows the live goroutine count; each
goroutine block shows its last-known wait state in brackets
(`running`, `chan receive`, `chan send`, `mutex wait`,
`io wait`, `timer`). The function-line pair is the user
function the runtime last recorded for that goroutine.

To diagnose a hung process:

1. Find the dominant wait state across goroutines.
2. If most are `chan receive` on the same channel, you have
   a producer-side bug (the producer goroutine is missing
   from the dump or itself blocked).
3. If most are `mutex wait`, look for the goroutine *not* in
   `mutex wait` — it holds the lock and is doing something
   the holder shouldn't be doing.

## 3. Running pprof

Mount `/debug/pprof/*` in your HTTP router by routing to
`std::pprof::route(path, query)` (see the deployment doc).
Then:

```sh
go tool pprof -text http://localhost:8080/debug/pprof/profile?seconds=30
go tool pprof -web  http://localhost:8080/debug/pprof/heap
go tool pprof       http://localhost:8080/debug/pprof/goroutine
```

The wire format is the legacy textual pprof shape, which
`go tool pprof` parses unchanged. The protobuf
(`profile.proto`) variant lands in Phase 2.

Available endpoints:

- `/debug/pprof/profile?seconds=N` — CPU profile over `N`
  seconds (default 30). Sampler ticks at ~100 Hz when active.
- `/debug/pprof/heap` — live allocation samples.
- `/debug/pprof/goroutine` — one entry per live goroutine.
- `/debug/pprof/mutex` — contention snapshot (Phase 2:
  populated once per-Mutex counters land).
- `/debug/pprof/block` — park/unpark snapshot (Phase 2).
- `/debug/pprof/` — index page listing the others.

## 4. Reading a `--race` report

Run the test suite under the race detector:

```sh
gos test --race
```

Each detected race surfaces as a single line:

```text
DATA RACE: addr=0xdeadbeef prev=17 (write) curr=42 (read)
```

`addr` is the heap address; `prev=N` is the goroutine that
performed the conflicting access; `curr=N` is the goroutine
whose access tripped the detector.

The detector seeds happens-before relations from the
scheduler's park/unpark events plus `Mutex::with` and
channel send/receive operations. Spurious reports are
unusual; if you see one, the most likely cause is a custom
synchronisation primitive that bypasses the standard
`std::sync` types — file an issue with the source.

## Cross-references

- [`deployment.md`](deployment.md) — observability stack
  for production.
- [`stability.md`](stability.md) — versioning + deprecation
  policy.
- [`stdlib.md`](stdlib.md) — `runtime`, `pprof`, `signal`.
