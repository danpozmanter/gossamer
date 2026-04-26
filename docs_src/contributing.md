# Contributing

## House rules

- Safe Rust only. Every crate forbids `unsafe_code`.
- `cargo clippy --workspace --all-targets -- -D warnings` is the
  gate.
- `cargo fmt --all --check` must pass.
- One focused change per PR. No drive-by refactors.

The full style guide is in
[`GUIDELINES.md`](https://github.com/danpozmanter/gossamer/blob/main/GUIDELINES.md).

## Getting oriented

- Start with the [language spec](https://github.com/danpozmanter/gossamer/blob/main/SPEC.md) for grammar and semantics.
- Each crate under `crates/` has a module-level `//!` doc
  describing the compiler phase it belongs to.
- Design notes live under
  [`docs/`](https://github.com/danpozmanter/gossamer/tree/main/docs)
  (perf baseline, binary-size baseline, diagnostics style guide,
  self-hosting study, incremental-compile rollout).

## Picking an issue

Streams ordered by impact, smallest to biggest:

1. Landing a new lint in `gossamer-lint` (the framework is in
   place; each lint is ~20 lines).
2. Wiring a new native stdlib module (see the `std::regex`
   wrapper for the pattern).
3. Extending the diagnostics style guide with acceptance
   fixtures.
4. Closing a security item in the roadmap.
5. Implementing a major language feature (Cranelift codegen,
   scheduler, LSP capabilities beyond the shipped slice).

## Running the tests

```sh
cargo test --workspace
```

Per-crate:

```sh
cargo test -p gossamer-mir
cargo test -p gossamer-std --lib
```

Release benches:

```sh
cargo test -p gossamer-interp --test perf_baseline --release -- --nocapture
```

## Docs

```sh
pip install mkdocs
mkdocs serve
```

Opens at `http://localhost:8000/`. Edit `docs_src/*.md` and save;
the site reloads.
