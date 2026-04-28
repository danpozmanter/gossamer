# Security

Gossamer's implementation forbids `unsafe` Rust workspace-wide
and audits every external dependency against a small approved
list. This page summarises the posture for users and points at
the hardening roadmap. Reporting details are in
[`SECURITY.md`](https://github.com/danpozmanter/gossamer/blob/main/SECURITY.md).

## What is done

- Zero `unsafe` in first-party code. Every crate carries
  `#![forbid(unsafe_code)]`.
- GC is arena-indexed, not pointer-based. Use-after-free
  through the GC API is representationally impossible.
- Minimal external dependencies: `anyhow`, `clap`,
  `codespan-reporting`, `parking_lot`, `thiserror` — plus
  `insta` as a dev-only snapshot tool.

## Known gaps

Before shipping production services on Gossamer, you should
know:

- The HTTP server enforces `max_header_bytes` (default 8 KiB)
  and `max_body_bytes` (default 1 MiB). Tune via `http::Config`
  if your traffic justifies a larger envelope; the defaults
  are deliberately conservative.
- `std::tls` is wired through `http::Server::bind_and_run_tls`
  and `http::Client::tls(...)`. mTLS, ALPN, and SNI are
  exposed. Reverse-proxy termination is no longer required.
- `crypto::rand::fill` uses `getrandom` and returns an explicit
  error if the OS RNG is unavailable. Callers must not
  silently discard that error in security-sensitive code.
- `os::env` / `os::args` / `os::set_env` work in both the
  interpreter and the compiled tier. Mutation paths
  (`set_env` / `unset_env`) route through
  `gossamer_runtime::safe_env` so they are safe to call before
  spawning goroutines.
- The data-race detector (`gos test --race`) catches
  unsynchronised concurrent writes via vector-clock
  happens-before analysis. CI gating on `--race` is
  recommended for any code that touches goroutines.

Open caveats tracked in
[`docs_src/non_goals_v1.md`](non_goals_v1.md):

- HTTP/2 + WebSockets are deferred.
- Per-line coverage instrumentation (Phase 2 follow-up) —
  the `--coverage` output today is at the test-file
  granularity.
- Postgres / MySQL drivers belong to the package ecosystem
  with their own maintainers and CVE response cadence.

## Reporting a vulnerability

Email security@gossamer-lang.org with a PoC and a suggested
severity. A `SECURITY.md` lands in the repository root alongside
the 1.0.0 release.

## CI automation (planned)

- `cargo deny` — license + advisory gate.
- `cargo audit` — weekly vulnerability scan.
- `cargo geiger` — unsafe-transitive usage snapshot.
- `cargo fuzz` — targets for lexer, parser, HTTP parser,
  manifest parser. Nightly run.
- `miri` — pure-Rust phases (diagnostics, MIR, lint) every PR.
