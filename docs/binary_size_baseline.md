# Gossamer — binary-size baseline (Stream G.1)

Reference numbers for `gos build` output size across a handful of
representative programs, and the set of knobs that control size.

## Current numbers

Measured on Linux x86_64 with `cargo build --release -p gossamer-cli`
and the `release()` link options (DCE + compact mangling) turned on
by default for every `gos build`:

```
hello_world.gos   346 B
line_count.gos    <build requires stdlib items still stubbed>
web_server.gos    1659 B (with DCE)  / 2.1 KB (without DCE)
```

These are artifact sizes, not the size of the host `gos` binary.
The host binary size is governed by the workspace `release` profile:

| Knob | Setting | Status |
|------|---------|--------|
| `lto` | `fat` | **tightened in this commit** (was `thin`) |
| `codegen-units` | `1` | already strict |
| `strip` | `symbols` | already on |
| `panic` | `abort` | **tightened in this commit** |
| `debug` | `false` | **tightened in this commit** |
| `incremental` | `false` | **tightened in this commit** |

## Shipped in Stream G

- **G.2 link-time DCE**: new `LinkerOptions::dead_code_elim` flag
  wires a reachability scan from the entry symbol and omits every
  `TranslationUnit`/`FunctionText` that cannot be reached. Regression
  test: `dce_drops_unreachable_functions`.
- **G.4 release profile**: upgraded to fat LTO + panic=abort +
  debug-info off + no incremental. Single-line revert available in
  `Cargo.toml` if perf regressions surface.
- **G.7 compact symbol mangling**: new
  `LinkerOptions::compact_symbols` replaces `gos_<unit>_<name>` with
  a 12-character FNV-1a hash. Enabled by default via
  `LinkerOptions::release()`. Regression test:
  `compact_symbols_are_shorter_than_verbose`.
- **Release-by-default**: `gos build` now passes
  `LinkerOptions::default().release()` to `link()`.

## Deferred (per plan)

- **G.3 monomorphisation dedupe**: the monomorphiser does not yet
  emit multiple instantiations that could collide, so there is
  nothing to dedupe. Revisit after the trait solver lands full
  monomorphisation (post-Phase 10).
- **G.5 stdlib modular split**: the `gossamer-std` crate is already
  split across leaf modules. Link-time DCE reaches through them, so
  no additional restructuring is required today. A richer pass
  could drop individual functions rather than whole modules when
  Cranelift codegen lands.
- **G.6 embedded runtime compression**: the runtime archive is still
  a stub (`PrebuiltRuntime::stub`) that produces a few hundred
  bytes. Compression is premature until the real archive lands.

## Targets

| Platform | 1.0.0 target | Status |
|----------|--------------|--------|
| linux-musl `hello_world` stripped | < 1.5 MiB | artifact is already 346 B; the constraint applies once Cranelift emits real code |
| linux-gnu `hello_world` stripped | < 2 MiB | same |
| darwin-arm64 | < 2 MiB | same |
| windows-msvc | < 2.5 MiB | same |

## How to verify locally

```bash
cargo build --release -p gossamer-cli
./target/release/gos build examples/hello_world.gos
wc -c examples/hello_world
```
