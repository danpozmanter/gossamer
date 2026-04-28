# Non-goals for v1

Gossamer's v1 release is deliberately scoped. The list below is
what is **not** in v1, with the reasoning. Each item lives in a
tracking issue or doc note — what's omitted is omitted on purpose,
not by oversight.

The goal is honesty: a developer evaluating Gossamer should know
in five minutes whether the gaps will block their use case.

## Language

- **No async / `await`.** Gossamer's concurrency model is
  Go-shaped goroutines + channels. `go expr` runs on real OS
  threads with cooperative scheduling. There is no plan for an
  `async` keyword in v1.
- **No macro system.** `println!("{name}")` interpolation is a
  parser-level special case, not user-extensible. Custom
  `derive` is post-v1.
- **No `unsafe` block.** First-party crates set
  `#![forbid(unsafe_code)]`. Raw pointers and inline assembly
  are not exposed to user code. The runtime's FFI boundary is
  the only `unsafe` in the workspace and is not user-reachable.
- **No const generics over arbitrary types.** Array sizes are
  literal `usize`. Generic parameters of type `T: const N: T`
  are not supported.

## Type system

- **No higher-kinded types.** Generic type constructors that
  themselves take type parameters (`Functor<F<_>>`) are out.
- **Generic monomorphization is flat-i64-ABI only in v1.** A
  generic `fn min<T: Ord>(a: T, b: T) -> T` works for `T` up to
  64 bits. Larger `T` (e.g. `String`, user structs) compile to a
  **diagnostic**, not garbage code, until layout-driven
  specialisation lands. See [`codegen_abi.md`](codegen_abi.md).
- **No specialisation, no negative impls, no GATs.** Plain
  bounded generics only.

## Runtime

The runtime gaps that this section originally listed are
**now in scope** — work-stealing M:N scheduler, async
preemption with GC safepoints, write barriers wired through
the LLVM lowerer, goroutine-aware sync primitives, and an
`epoll` / `kqueue` / `IOCP` netpoller all landed in the
production-readiness pass. The remaining runtime non-goals:

- **No generational GC.** The current collector is concurrent
  mark-sweep with a write barrier. A generational variant
  would help long-lived service workloads further; it is
  filed for Phase 2.
- **No stack-switching coroutines.** Goroutines are
  cooperative state machines on top of the work-stealing
  scheduler — no per-goroutine OS stack. Switch cost is
  function-call shaped, not register-flush shaped. A
  stackful coroutine variant requires `unsafe` and is
  outside the workspace's safe-Rust posture.

## Standard library

The following Go stdlib equivalents shipped during the
production-readiness pass and are no longer non-goals:

- ✅ `database/sql` interface + bundled SQLite driver
  (`std::database::sql`).
- ✅ Locale-aware / IANA-tz time formatting via
  `std::time::tz`.
- ✅ TLS client-cert verification, ALPN, SNI, mTLS through
  both `http::Client` and `http::Server`.

Still deliberately deferred:

- HTTP/2 server + client. The `0.x` series ships HTTP/1.1.
- WebSockets. Phase-2 stdlib addition.
- gRPC. Lands via a third-party package once the registry
  publishing flow opens.
- Postgres / MySQL drivers. Third-party — drivers belong
  with their own maintainers and CVE response cadence.
- `encoding/csv`, `encoding/xml`. JSON + YAML cover the
  config / interchange surface for now.

## Tooling

- **No `gos publish` flow yet.** Authoring a package and sharing
  it depends on three pieces — a deterministic packer, an
  ed25519-signing seam, and a registry-topology decision. We've
  built the packer in `gossamer-pkg::tar::pack`; the rest waits
  on real demand and on landing the crypto seam (`ring`-backed
  ed25519). Until then, the consumer side (`gos add` /
  `gos fetch` against a `url + sha256` snippet) works and is
  enough to depend on a third-party tarball you've hosted by
  hand.
- **No package registry server.** Discovery, namespaces,
  deprecation flow, and CDN-backed downloads are post-v1.
- **No production-grade benchmarking framework.** `gos bench`
  exists but does not run statistically rigorous comparisons or
  emit machine-readable JSON. Treat the numbers as developer
  feedback, not CI gates.
- ✅ **Coverage tooling shipped (test-file granularity).**
  `gos test --coverage out.lcov` emits well-formed lcov.
  MIR-level basic-block instrumentation that would let us
  compute per-line hits is filed for Phase 2.
- **No ergonomic editor extensions beyond LSP.** Gossamer
  ships an LSP 3.16 server. VS Code / Neovim / Emacs / Helix
  pick it up via the standard generic-LSP plugins.

## Security

- **No sandboxed execution.** `gos run` and compiled binaries
  have full host access. Capability-restricted execution is a
  v1.x feature.
- **No `#[deprecated]` lint over user types.** Available on
  stdlib items only via doc comments.

## What this list is *not*

This is not a list of bugs or unfinished features that *should*
be in v1. Items here are **explicit deferrals**: shipping the
language without them is a feature, not a regret. Each was
weighed against v1 calendar pressure and the team's bet that
the items on the list are the right ones to defer.

If you need one of these, file an issue tagged `v1.x-pending`
with a use-case. Tickets with concrete user demand drive the
v1.x priority order.

## Cross-references

- [`codegen_abi.md`](codegen_abi.md) — flat-i64 monomorphisation.
- [`migration/go.md`](migration/go.md) — how the Go-stdlib gaps
  affect migration.
- [`stdlib.md`](stdlib.md) — what the stdlib *does* cover.
