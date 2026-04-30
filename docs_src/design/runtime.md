# Runtime internals

This page is a map — not a specification — of what happens between
`gos run` and `main` returning. Each section links to the crate that
owns the stage so a new contributor can find the real source.

## Stages

```
source.gos
   │
   ▼
┌──────────────┐  gossamer-lex        tokens + source map
│  Lexing      │
└──────────────┘
   │
   ▼
┌──────────────┐  gossamer-parse      AST (items + uses)
│  Parsing     │  gossamer-ast        + diagnostics
└──────────────┘
   │
   ▼
┌──────────────┐  gossamer-resolve    name resolution, imports
│  Resolution  │                      path → DefId mapping
└──────────────┘
   │
   ▼
┌──────────────┐  gossamer-types      type inference, trait solve,
│  Type check  │                      exhaustiveness
└──────────────┘
   │
   ▼
┌──────────────┐  gossamer-hir        lowered program tree
│  HIR lower   │                      (match-desugars, for → loop)
└──────────────┘
   │
   ▼
┌──────────────┐  gossamer-interp     tree-walker VM; the bytecode
│  Evaluation  │                      and Cranelift backends live in
└──────────────┘                      gossamer-mir / -codegen-*.
```

## Evaluator

The tree-walker in `gossamer-interp` is the default engine. It:

1. Accepts an `HirProgram`.
2. Installs every top-level function and inherent-impl method under
   both the unqualified (`foo`) and type-qualified (`Type::foo`)
   names in a `HashMap<String, Value>`.
3. Registers builtin callables for stdlib functions (`os::args`,
   `time::sleep`, `json::parse`, …) and variant constructors for
   every user enum.
4. Walks HIR expressions directly, keeping local bindings in an
   `Env` stack.

Struct values are `Rc<Vec<(Ident, Value)>>`. Field assignment runs
through a copy-on-write helper that allocates a fresh `Rc` so alias
bindings never observe each other's mutations.

## Garbage collector

`gossamer-gc` is the off-line design of the real concurrent GC — a
tri-colour mark-sweep collector with write-barriers for generations
and weak references. The tree-walker currently piggy-backs on Rust's
`Rc` / `Arc` reference counting; the concurrent GC comes online once
the Arc-based interpreter lands.

## Scheduler

`gossamer-runtime::sched` is the work-stealing M:N scheduler.
Every Gossamer binary links it through `libgossamer_runtime.a`, so
`go expr` in compiled code (and the bytecode VM) lands on the same
shared pool. The pool size defaults to `num_cpus()`, overridable
via `GOSSAMER_MAX_PROCS=N` or `runtime::set_max_procs(n)` from
user code.

A `MultiScheduler` owns:

- one work-stealing deque per worker M (a `crossbeam_deque::Worker`),
- a global injector (`crossbeam_deque::Injector`) for cross-thread
  pushes and the netpoller's wakeup path,
- a `parked` map keyed by `Gid` for goroutines suspended on I/O,
  channels, mutexes, sleeps, or the blocking-syscall pool,
- a watchdog thread that bumps the cooperative preempt phase
  every 5 ms and signals SIGURG to a worker that's been running
  more than 100 ms.

## Goroutines

`go expr(args)` is a real stackful coroutine. Construction:

1. `gossamer_runtime::sched_global::spawn(closure)` allocates a
   16 KiB `corosensei::Coroutine` stack (override:
   `GOSSAMER_GOROUTINE_STACK=N`).
2. The coroutine's entry shim publishes its `Yielder` pointer to a
   shared slot, sets the worker's TLS yielder, then runs `closure`.
3. The scheduler wraps the coroutine in a `GoroutineTask` whose
   `step()` calls `coroutine.resume()`. Result `Yield` →
   `Step::Yield`; result `Return` → `Step::Done`.

When user code blocks (channel recv on empty, mutex contention,
`time::sleep`, `net::TcpStream::read` returning `WouldBlock`,
filesystem syscall via `blocking_pool::run`), the helper calls
`sched_global::park(reason, |parker| { register parker.gid with
the wakeup source })` and then `gossamer_coro::suspend()`. The
worker M sees `Step::Yield` plus a pending-park flag and moves
the task into `MultiScheduler::parked` keyed by gid. The wakeup
source (poller readiness, channel send, mutex unlock, blocking-pool
worker, ...) calls `MultiScheduler::unpark(gid)` which pushes the
task back onto the injector. Any free worker picks it up and
resumes the coroutine — possibly on a different OS thread than the
one it suspended on.

A blocked goroutine costs ~16 KiB of mmap'd stack, not an OS
thread. 10 000 idle goroutines fit on a 4-worker scheduler in
roughly 160 MiB of address space.

The wake-before-park race window (where `unpark(gid)` arrives
before the goroutine has actually been moved into `parked`) is
closed by a `pre_unpark` set: if `unpark(gid)` finds the gid not
yet parked, it records the gid; the worker about to park the
task observes the pre-unpark and immediately re-ejects the task
to the injector instead of leaving it parked.

## Netpoller

`gossamer-runtime::sched::poller::OsPoller` wraps `mio` (epoll on
Linux, kqueue on macOS / BSD, IOCP on Windows). One dedicated
`gos-netpoller` OS thread blocks on `OsPoller::poll(50 ms)` and
dispatches each readiness event to the goroutine that registered
for it via `register_waker(gid, closure)`. Default closure: just
`scheduler().unpark(gid)`. Timers (`time::sleep`,
`http::Client::do_request` deadlines) ride the same wheel.

## HTTP server

`gossamer-std::http::server::run` and the compiled-tier
`gos_rt_http_serve` both:

- bind a non-blocking `TcpListener`,
- park on the netpoller for accept readiness,
- spawn each accepted connection as a goroutine via
  `sched_global::spawn`,
- read / write under the netpoller — `WouldBlock` parks the
  goroutine; the worker thread immediately picks up another
  connection.

Graceful shutdown is driven by:

- `GOSSAMER_HTTP_MAX_REQUESTS=N` — env var, stop after N requests.
- `gossamer_interp::set_http_max_requests(N)` — safe-Rust test hook.
- `config.shutdown: Arc<AtomicBool>` — for in-process callers.

## Panic recovery

`panic(msg)` in user code returns `RuntimeError::Panic(msg)` from
the evaluator. The native HTTP server catches that per-request,
logs it, and returns a 500. A panic inside a goroutine body
unwinds the coroutine's stack and propagates to the worker M's
resume site — the worker exits with the panic, but other
goroutines on other workers continue running. A program-wide
panic handler can be installed via `panic::set_hook` from user
code.

## Where each stage is tested

| Stage | Test location |
|-------|---------------|
| Lexing | `gossamer-lex/tests/` |
| Parsing | `gossamer-parse/tests/` |
| Resolution | `gossamer-resolve/tests/smoke.rs` |
| Type check | `gossamer-types/tests/typeck.rs`, `tests/exhaustiveness.rs` |
| HIR lower | `gossamer-hir/tests/lower.rs` |
| Interpreter | `gossamer-interp/tests/{eval,run_pass,vm,http_end_to_end}.rs` |
| Stdlib | `gossamer-std/src/*` (`#[cfg(test)]` modules) |
| Driver | `gossamer-driver/tests/` |
| CLI | `gossamer-cli/tests/cli.rs` |
