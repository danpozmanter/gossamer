# Changelog

## 0.0.0

Initial release. Not production ready.

### Runtime — true M:N goroutines (2026-04-30)

- Goroutines are now stackful coroutines built on
  [`corosensei`](https://crates.io/crates/corosensei). Every
  `go fn(args)` runs on a dedicated 16 KiB mmap'd stack — no
  OS-thread-per-goroutine model, no `async` colouring on the
  language surface.
- `gossamer-runtime::sched_global::park(reason, |parker| ...)`
  is the universal park primitive. The arm closure registers
  the parker's `gid` with whatever wakeup source the goroutine
  is waiting on (a poller waker, a channel parked-receivers
  list, a mutex queue, the blocking-syscall pool's completion
  hook, ...) and `gossamer_coro::suspend()` yields control to
  the scheduler. The wakeup source's `MultiScheduler::unpark(gid)`
  resurrects the goroutine on any worker.
- The wake-before-park race window is closed by a `pre_unpark`
  set: an `unpark(gid)` that arrives before the worker has moved
  the task into `parked` is recorded; the worker observes and
  immediately re-ejects the task to the injector instead of
  leaving it parked.
- Every blocking primitive in stdlib was rewired to call `park`
  instead of OS-blocking the worker M: `time::sleep`,
  `net::TcpStream::read/write_all`, `net::TcpListener::accept`,
  `http_request` (compiled HTTP client), `gos_rt_chan_send`,
  `gos_rt_chan_recv`, `sync::Mutex.with`, `sync::WaitGroup.wait`,
  and `blocking_pool::run` (filesystem / exec syscalls).
- `MultiScheduler::enter_blocking_syscall` /
  `resume_from_blocking_syscall` are gone — the OS-thread-fanout
  hack is no longer needed. Same for the `set_spawn_handler`
  shim and the `OneShot` adapter.
- Configuration env vars: `GOSSAMER_MAX_PROCS=N` sets worker count
  (default `num_cpus()`). `GOSSAMER_MAX_GOROUTINES=N` caps live
  goroutines (default 1 000 000). `GOSSAMER_GOROUTINE_STACK=N`
  overrides the default 16 KiB coroutine stack size.

### Supported platforms

Stackful coroutines need a per-arch context-switch implementation,
so the support matrix is narrower than "anything Rust builds":

- Linux on x86_64, aarch64, armv7
- macOS on x86_64 (Intel) and aarch64 (Apple Silicon)
- Windows on x86_64 (MSVC ABI)

Other targets compile but the goroutine scheduler refuses to start.
