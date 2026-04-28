# Stability and compatibility policy

Drafted as the policy that takes effect at the first stable
tag. The current version (0.0.0) is still pre-stable; this
document is the contract the project is preparing to honour.
Adapted from Go 1's compatibility promise; trimmed to what
makes sense for a pre-1.0 project.

## What this document promises

For every minor release in the `0.x` series:

- **Source-level compatibility** for code written against the
  documented public API of the language, the standard library
  (modules listed in `gossamer-std`'s `manifest.rs`), and the `gos`
  CLI subcommand surface.
- **Wire-format compatibility** for serialised formats the tool
  emits as deliverables: pprof profiles, lcov coverage, JUnit XML,
  CycloneDX SBOM. A consumer that worked against `0.x.y` must keep
  working against `0.x.(y+1)`.
- **Build-config compatibility** for `project.toml` keys we
  document as stable. Adding a new optional key is fine; renaming
  or removing one is a breaking change.

## What this document explicitly does not promise

- **Runtime performance characteristics**: throughput numbers,
  steady-state memory, GC pause distributions may move in either
  direction across minor versions. We will document material
  regressions in release notes.
- **MIR / HIR layout**: the in-memory IR types are
  implementation details. Programs that depend on them via
  reflection-style introspection are on their own.
- **`gossamer-runtime` C-ABI symbols**: the `gos_rt_*` surface is
  internal to the toolchain; user code calling those symbols
  directly through FFI may break across releases.
- **Compiled object compatibility**: an object file emitted by
  `gos build --release` of `0.x.y` is not guaranteed to link
  against the runtime of `0.x.(y+1)`. Re-build downstream programs
  on toolchain upgrades.

## Versioning rules

Following [Semantic Versioning](https://semver.org/) with the
following project-specific clarifications:

- A **patch** release (`0.x.y` → `0.x.(y+1)`) ships bug fixes,
  performance improvements, and additive optional fields. It
  must not break a program that compiled against `0.x.y` or
  change the documented behaviour of that program.
- A **minor** release (`0.x` → `0.(x+1)`) ships new APIs and
  features. Old code should still compile unchanged. The release
  notes call out anything we judge a soft break (e.g. a stricter
  lint that fires by default).
- The **major** release that takes us to `1.0.0` locks the
  language and stdlib surface; future breaking changes wait for
  `2.0.0`.

## Deprecation process

When an API needs to go away:

1. **Announce** in the release notes. Mark the symbol with the
   `@deprecated("explanation, replacement")` doc attribute. The
   compiler emits a warning lint when the symbol is used.
2. **One full minor cycle** after the warning lands, the symbol
   becomes a hard error in the next minor.
3. **One additional minor cycle** later, the symbol is removed.

Upshot: an API marked deprecated in `0.5` is gone in `0.7`. Users
who track minor releases get one full cycle of compile-time
warnings before any code stops working.

## What counts as a public API

For the purposes of this document, the public API is:

- Every `pub` item in a module the manifest at
  `crates/gossamer-std/src/manifest.rs` registers — _and only_
  those modules. Items reachable from a manifest-registered module
  but not registered themselves are implementation details.
- Every CLI subcommand listed in `gos --help`. Adding a new flag
  to an existing subcommand is fine; removing one is a break.
- The wire formats listed under "What this document promises".

The Rust-internal `crates/gossamer-*` API surface is not public
even though it is `pub` for inter-crate visibility. Consumers
linking against the workspace as a Rust dependency are operating
outside this stability promise.

## Security exception

A security fix that requires a breaking API change ships in the
next patch release with the break documented in the release
notes. We do not delay a CVE response for the deprecation window.

## Current status

The workspace is at version 0.0.0. This document describes
the compatibility contract the project is preparing to adopt
at the first stable tag; until then, callers should treat the
public API as may-change-with-notice. Use the release notes
on each tag to track diff.
