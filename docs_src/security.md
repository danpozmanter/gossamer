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

- The HTTP server does not bound header / body size today. An
  attacker can send an unbounded stream and OOM the process.
  **Do not expose the default server past a trust boundary.**
- `std::tls` is not yet implemented. Every built-in HTTP
  listener is cleartext. Use a reverse proxy (nginx, Caddy) for
  TLS termination in the interim.
- `crypto::rand::fill` only works on Linux today. On other
  platforms it returns an `Err`; callers that discard the error
  will read zeros. Do not ship key generation on non-Linux
  hosts yet.
- `os::env`, `os::args`, and the filesystem stubs are no-ops in
  the current interpreter. Programs that assume they work get
  silent Unit values.

Each item is tracked with a remediation PR on the security
hardening backlog.

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
