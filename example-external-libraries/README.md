# Gossamer external-library examples

Two end-to-end demonstrations of `[rust-bindings]` in a Gossamer
project:

- `01-gossamer-aware/` — a local Rust crate that depends on
  `gossamer-binding` and uses `register_module!` to expose three
  helpers (`shout`, `sum`, `stats`) under the `echo` Gossamer
  module. This is the path tuigoose, sqlite-binding, etc. take.

- `02-plain-rust-wrapped/` — wraps the published `unic-segment`
  crate (which knows nothing about Gossamer) by introducing a
  thin `gos-unic-segment` wrapper crate under
  `.gos-bindings/unic-segment/`. The wrapper does the
  `register_module!` dance and re-exports two helpers
  (`graphemes`, `grapheme_count`) under the `unic_segment`
  Gossamer module.

Both examples exercise the same toolchain plumbing: the on-PATH
`gos` binary detects `[rust-bindings]` in `project.toml`, Cargo
builds a per-project runner that statically links every binding,
and the runner re-enters `gossamer_cli::run_main` with every
binding installed in the interpreter / type-checker.

## Running the examples

```sh
# Build a fresh `gos` from the gossamer source tree.
cargo build -p gossamer-cli

# Either run them one at a time…
( cd 01-gossamer-aware && /path/to/gos run src/main.gos )
( cd 02-plain-rust-wrapped && /path/to/gos run src/main.gos )

# …or use the helper script which does both.
bash run_examples.sh
```

The first invocation against a project takes ~30–60 s while
Cargo builds the runner; subsequent runs reuse the cached build
under `$XDG_CACHE_HOME/gossamer/runners/`.

## Compiled-mode build

`gos build src/main.gos` links the runtime, the user code, and
`libgos_static_bindings.a` (built per-project from the same
binding spec) into a single static binary. Codegen lowering for
binding calls is incremental — the binary builds and runs, and
calls that go through the runtime VM (the `gos run` path) work
end-to-end. Direct compiled-tier dispatch into binding C-ABI
thunks is wired through cranelift / LLVM as more binding shapes
land.
