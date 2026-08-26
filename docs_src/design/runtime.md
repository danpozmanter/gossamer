# Runtime internals

This page is a map - not a specification - of what happens between
`gos` and `main` returning. Each section links to the crate that
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
┌──────────────┐  gossamer-interp     register-based bytecode VM;
│  Evaluation  │                      the LLVM and Cranelift backends
└──────────────┘                      live in gossamer-mir / -codegen-*.
```

## Evaluator

The register-based bytecode VM in `gossamer-interp` is the sole
`gos` / `gos test` engine. It:

1. Accepts an `HirProgram`.
2. Compiles every top-level function and inherent-impl method to a
   register-machine `FnChunk`, registered under both the unqualified
   (`foo`) and type-qualified (`Type::foo`) names.
3. Registers builtin callables for stdlib functions (`env::args`,
   `time::sleep`, `json::parse`, …) and variant constructors for
   every user enum.
4. Executes the compiled bytecode in a register machine, keeping
   locals in per-frame register files. Every construct - closures,
   `select`, `defer`, or-patterns, goroutines, custom iterators - is
   lowered to native bytecode; nothing is interpreted from HIR.

Struct values are `Vec<(Ident, Value)>`. Field assignment runs
through a copy-on-write helper that allocates a fresh `Rc` so alias
bindings never observe each other's mutations.

## Memory management

Compiled programs manage memory deterministically with a Swift-like
model - reference counting plus arenas, and no tracing collector:

- **Reference counting** (`gossamer-runtime::c_abi::rc`). Recursive
  heap enums and runtime containers carry an intrusive
  `[RcHeader | payload]` header. Codegen emits balanced
  retain/release pairs; a strong count hitting zero releases the
  value's reference-counted children iteratively and frees the
  payload. Weak references follow the Swift-ARC model.
- **Cycle collection.** The compiled tiers run a Bacon-Rajan
  trial-deletion pass over suspected cycle roots, both on demand
  (`runtime::collect_cycles()`) and automatically under allocation
  pressure. The bytecode VM backs values with `Arc` and does not
  collect cycles, so `collect_cycles()` is a no-op there: a strong
  reference cycle leaks under `gos` but is reclaimed under
  `gos build`.
- **Aggregate reclamation.** Structs / tuples / arrays are
  heap-allocated via `gos_rt_aggr_alloc` (plain zeroed malloc) and
  freed by the MIR drop pass via `gos_rt_aggr_free` at scope exit.
- **Arenas.** `arena { }` blocks bump-allocate and free wholesale
  at every block exit.

The bytecode VM piggy-backs on Rust's `Rc` / `Arc` for object
lifetime; its output is the semantic oracle the compiled tiers
match, cross-checked by the tier-parity suite and the VM-vs-LLVM-AOT
differential.

## Scheduler

`gossamer-runtime::sched` is the work-stealing M:N scheduler.
Every Gossamer binary links it through `libgossamer_runtime.a`, so
`spawn(|| expr)` in compiled code (and the bytecode VM) lands on the same
shared pool. The pool size defaults to `num_cpus()`, overridable
via `GOSSAMER_MAX_PROCS=N` or `runtime::set_max_procs(n)` from
user code.

Deferred JIT promotion reports compile time, emitted native-code bytes, and
process RSS through VM diagnostics. Set `GOS_JIT_MAX_CODE_BYTES=N` to reject a
promotion whose retained code would exceed `N` bytes for that VM; set
`GOS_JIT_MAX_RSS_MB=N` to reject a promotion before compilation when the
process is already at its RSS budget. A rejected artifact releases its MIR
snapshot and remains on bytecode.

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

`spawn(|| expr(args))` is a real stackful coroutine. Construction:

1. `gossamer_runtime::sched_global::spawn(closure)` allocates a
   1 MiB `corosensei::Coroutine` stack (override:
   `GOSSAMER_GOROUTINE_STACK=N`). The stack is an `mmap` reservation
   fronted by a guard page; the OS commits pages on first touch, so
   resident memory tracks the depth a goroutine actually uses rather
   than the reservation. See [Goroutine stack model](goroutine_stacks.md).
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
task back onto its home worker's inbox. A stackful coroutine cannot
migrate between OS threads while suspended, so a goroutine resumes on
the worker it first landed on.

A blocked goroutine costs only the few KiB of stack its shallow
frame actually touched - not an OS thread, and not the 1 MiB
reservation. Thousands of concurrent goroutines stay in the tens of
MiB of resident memory; the 1 MiB reservations consume address space
(abundant on 64-bit) that never becomes resident until used. The
[Goroutine stack model](goroutine_stacks.md) note explains why this
guard-page / lazy-commit scheme was chosen over Go-style copyable
stacks.

The wake-before-park race window (where `unpark(gid)` arrives
before the goroutine has actually been moved into `parked`) is
closed by a `pre_unpark` set: if `unpark(gid)` finds the gid not
yet parked, it records the gid; the worker about to park the
task observes the pre-unpark and immediately re-ejects the task
to the injector instead of leaving it parked.

## Preemption

Scheduling is **cooperative**, with a watchdog that requests a yield and, past
a longer threshold, interrupts the worker's syscalls. There is no asynchronous
preemption: nothing moves a running goroutine off its worker without that
goroutine reaching a safepoint. This is the one place the runtime is weaker
than Go 1.14 and later, whose signal handler relocates a goroutine from a
loop that never yields.

A goroutine yields the worker M at *safepoints*:

- every park point - channel send/recv, `select`, mutex contention,
  `time::sleep`, scheduler-aware network reads, and core filesystem operations,
- function-call / scheduler-step boundaries, where the worker can
  reclaim the coroutine between `step()` invocations,
- loop back-edges **on the bytecode VM**, which polls every 1,024 taken
  back-edges and yields its OS worker when the watchdog phase has changed.

The compiled tiers do **not** poll loop back-edges. `emit_preempt_check` on the
LLVM path is deliberately a no-op and the Cranelift JIT emits no back-edge
poll: the opaque runtime call and its countdown state block the optimizers on
exactly the numeric loops those backends exist to recover. `gos_rt_preempt_check`
and `gos_rt_preempt_check_and_yield` stay in the ABI for when a cheaper
safepoint shape lands.

The watchdog thread (`sched::multi::watchdog_loop`, 5 ms tick) escalates
against a worker that has not reached a safepoint:

- after ~10 ms it bumps the global *preempt phase*
  (`preempt::request_yield_all`); the next `preempt::should_yield`
  poll at any safepoint returns `true` and the goroutine yields,
- after ~100 ms it sends a real OS signal to that worker's thread -
  `SIGURG` on Unix, a `QueueUserAPC` on Windows. The signal does not itself
  context-switch. It flips the yield flag and interrupts a blocking syscall the
  worker is stuck inside (the kernel returns `EINTR`), which is what rescues a
  worker blocked in a non-scheduler-aware call. A goroutine spinning in a
  call-free compiled loop reads no flag, so the signal does not dislodge it.

### What this means for a program

A CPU-bound loop that calls nothing - no function call, no allocation, no
channel or timer operation - holds its worker until it finishes:

- on the bytecode VM it still yields, through the back-edge poll;
- in a `gos build` binary, and in a JIT-compiled body, it does not.

With `GOMAXPROCS`-many workers this starves one worker, not the program: peers
keep running. It becomes visible when such a loop outnumbers the workers, or
when a single-worker configuration runs one. Give a long computation a
safepoint the way you would give it a cancellation point - call
`runtime::cohort_cancelled()`, or any other function, on an outer iteration -
and the scheduler reclaims the worker at that call.

A VM configured with one goroutine worker can still be monopolized by one
nonterminating task; replacing that pool with resumable VM frames remains a
tracked limitation.

Not every host operation is scheduler-aware yet. Core filesystem file and path
I/O, HTTP client work, channels, timers, and socket readiness are routed or
parked. Specialty filesystem bridges, some process pipe operations, compression,
terminal calls, and third-party binding code still require effect-ledger audit.

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
- read / write under the netpoller - `WouldBlock` parks the
  goroutine; the worker thread immediately picks up another
  connection.

Graceful shutdown is driven by:

- `GOSSAMER_HTTP_MAX_REQUESTS=N` - env var, stop after N requests.
- `gossamer_interp::set_http_max_requests(N)` - safe-Rust test hook.
- `config.shutdown: AtomicBool` - for in-process callers.

## Panic recovery

`panic(msg)` in user code returns `RuntimeError::Panic(msg)` from
the evaluator. The native HTTP server catches that per-request,
logs it, and returns a 500. A panic inside a goroutine body
unwinds the coroutine's stack and propagates to the worker M's
resume site - the worker exits with the panic, but other
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
