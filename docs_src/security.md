# Security

Gossamer's implementation forbids `unsafe` Rust workspace-wide
and audits every external dependency against a small approved
list. This page summarises the posture for users and points at
the hardening roadmap. Reporting details are in
[`SECURITY.md`](https://github.com/danpozmanter/gossamer/blob/main/SECURITY.md).

## What is done

- The compiler front-end forbids `unsafe`. The parser, resolver,
  type checker, MIR, LLVM codegen, lints, diagnostics, LSP, and
  scheduler crates all carry `#![forbid(unsafe_code)]`. The runtime,
  the Cranelift JIT, the stackful-coroutine layer, and the FFI
  binding surface contain contained, reviewed `unsafe` - unavoidable
  for C ABIs, executing generated machine code, and context
  switching - kept in the smallest possible scope.
- No manual memory management in the language: there is no
  `free`, no raw pointers, and no `unsafe` keyword in Gossamer
  source. Memory is reclaimed automatically by reference counting on every
  tier, with a thread-local cycle collector on compiled tiers. The VM and
  cross-goroutine object graphs can retain strong cycles, so use `Weak<T>` when
  the graph can cycle. Use-after-free and
  double-free are not expressible in ordinary Gossamer code. The
  one escape hatch is the low-level `runtime::arena_push()` /
  `arena_pop()` primitive; its `arena { }` block form is statically
  escape-checked (`error[GM0003]`), but the raw calls are not.
- A curated set of well-known external crates, each reviewed before
  adoption: `clap`, `serde` / `serde_json`, `toml`, `parking_lot`,
  the `crossbeam` channels / deques, `rayon`, `mio`, the `unicode-*`
  family, `sha2`, `ring` / `rustls` for TLS, `regex`, Cranelift and
  LLVM for codegen, and `corosensei` for stackful coroutines - with
  `insta` as a dev-only snapshot tool.

## Known gaps

Before shipping production services on Gossamer, you should
know:

- The HTTP server enforces `max_header_bytes` (default 8 KiB)
  and `max_body_bytes` (default 1 MiB). Tune via `http::Config`
  if your traffic justifies a larger envelope; the defaults
  are deliberately conservative.
- `std::tls` is wired through `http::serve_tls` and
  `net::TcpStream` TLS upgrades. TLS configuration constructors
  are host-runtime internals, not Gossamer callables.
- `crypto::rand::fill` uses `getrandom` and returns an explicit
  error if the OS RNG is unavailable. Callers must not
  silently discard that error in security-sensitive code.
- `env::var` / `env::args` / `env::set_var` work in both the
  interpreter and the compiled tier. Mutation paths
  (`set_env` / `unset_env`) route through
  `gossamer_runtime::safe_env` so they are safe to call before
  spawning goroutines.
- The data-race detector (`gos test --race`) catches
  unsynchronised concurrent writes via vector-clock
  happens-before analysis. CI gating on `--race` is
  recommended for any code that touches goroutines.

Open caveats:

- HTTP/2 + WebSockets are deferred to v1.x.
- Per-line coverage instrumentation (Phase 2 follow-up) -
  the `--coverage` output today is at the test-file
  granularity.
- Postgres / MySQL drivers belong to the package ecosystem
  with their own maintainers and CVE response cadence.

## Reporting a vulnerability

Email security@gossamer-lang.org with a PoC and a suggested
severity. A `SECURITY.md` lands in the repository root alongside
the 1.0.0 release.

## CI automation

- `cargo deny` and `cargo audit` run on pull requests and main.
- The pull-request fuzz smoke covers lexer, parser, manifest, HTTP,
  resolver, HIR, type, MIR, bytecode-compile, and bytecode-run targets;
  longer bounded fuzz runs are scheduled weekly.
- A pinned-nightly Miri suite runs weekly for the runtime, scheduler,
  coroutine, resolver, type, and MIR crates. It is intentionally scoped to
  code Miri can execute.
- ASan runs on the runtime, interpreter, coroutine, MIR, and binding crates;
  TSan covers the runtime, scheduler, and coroutine crates on main and on a
  nightly schedule.
