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

- **One OS thread per goroutine.** Real M:N scheduling is a
  v1.x optimisation. The current model is correct but not as
  cheap as Go's scheduler. Document the constraint, ship.
- **No green-thread-aware blocking I/O.** Calls into the host
  block the OS thread. Spawn another goroutine to make progress.
- **GC pauses are stop-the-world.** A concurrent / generational
  GC is post-v1. The stop-the-world mark-sweep is fine for
  typical service workloads under tens of thousands of live
  goroutines, but a real-time game loop is not the target.

## Standard library

The following Go stdlib equivalents are **deliberately deferred
to v1.x**:

- HTTP/2 server + client. v1 ships HTTP/1.1 only.
- gRPC. Land via a third-party package once the registry is
  open for publishing.
- `database/sql`-shaped driver interface plus PostgreSQL /
  MySQL drivers. Third-party.
- Locale-aware time formatting (`time::format_locale`).
- Full TLS client cert verification + ALPN beyond `h2`. The
  `rustls` handle is exposed; the high-level convenience API
  is in v1.x.
- `encoding/csv`, `encoding/xml`. v1 ships JSON only.

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
- **No coverage tooling.** `gos test` runs the test suite; it
  does not emit lcov / cobertura output.
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
