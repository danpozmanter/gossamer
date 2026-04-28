# Gossamer

[![CI](https://github.com/danpozmanter/gossamer/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/danpozmanter/gossamer/actions/workflows/ci.yml)

[Homepage and Docs](http://gossamer-lang.org/)

A garbage-collected, goroutine-powered, fast-compiling systems
language with Rust's surface syntax, Go's runtime, and the forward pipe operator.

- Language spec: [`SPEC.md`](SPEC.md)
- Project style guide: [`GUIDELINES.md`](GUIDELINES.md)
- AI skill card: [`SKILL.md`](SKILL.md) — drop this file into a model's context to teach it how to write idiomatic Gossamer (also embedded in `gos skill-prompt`).
- Toolchain + stdlib + lint reference: [`docs_src/`](docs_src/) (built into `docs/`, served by GitHub Pages at <https://danpozmanter.github.io/gossamer/>)
- Editor integrations: [`danpozmanter/gossamer-editor-support`](https://github.com/danpozmanter/gossamer-editor-support) (VSCode, Vim, Neovim, Helix, Emacs, Sublime, Zed, plus a tree-sitter grammar)
- Contributing: [`CONTRIBUTING.md`](CONTRIBUTING.md)

Source files use the `.gos` extension. The CLI is `gos`. Manifests
live in `project.toml`.

Not yet stable. Everything here is subject to change without notice.

## Gossamer's Syntax

Gossamer leans on a forward-pipe operator (`|>`) so data flows
left-to-right. `x |> f(a, b)` desugars to
`f(a, b, x)`, and `|>` chains cleanly with methods, closures, and
plain functions:

```gossamer
fn double(x: i64) -> i64 { x * 2 }
fn add(a: i64, b: i64) -> i64 { a + b }
fn clamp(lo: i64, hi: i64, x: i64) -> i64 {
    if x < lo { lo } else if x > hi { hi } else { x }
}

fn main() {
    // 3 -> double -> add 10 -> clamp to [0, 100]
    let n = 3 |> double |> add(10) |> clamp(0, 100)
    println("arithmetic:", n)

    // Methods pipe the same way.
    let words = "  Hello  World  "
        |> str::trim
        |> str::to_lowercase
        |> str::split(" ")
        |> iter::count

    println("words:", words)
}
```

A goroutine + channel sketch:

```gossamer
fn main() {
    let (tx, rx) = channel::<i64>()
    go fn() { tx.send(40 |> add(2)) }()
    println("answer:", rx.recv())
}
```

## Language at a glance

Gossamer draws the feel of Rust and the runtime of Go, plus a
handful of ergonomic ideas picked off from Python:

- **Syntax.** Rust-flavoured. `fn`, `let`/`let mut`, `match`,
  `struct`, `enum`, `trait`, `impl`. Expressions-as-statements.
  Pattern-matching with guards, or-patterns, and rest-patterns.
  Tuple / struct destructuring in `let` and match arms.
- **Types.** Static, inferred at the statement level. `i8` → `i128`,
  `u8` → `u128`, `isize`, `usize`, `f32`, `f64`, `bool`, `char`,
  `String`, `[T]`, `(A, B, …)`, `Option<T>`, `Result<T, E>`,
  references (`&T` / `&mut T`), generics with trait bounds.
- **Type system.** Rust-shaped: nominal `struct`/`enum`, explicit
  generics, trait-based ad-hoc polymorphism, pattern matching
  with exhaustiveness, unit and never types in the interner.
  Inference is Hindley–Milner-flavour (union-find, structural
  unification, occurs check) but without let-polymorphism —
  polymorphism flows through explicit generics, not `let`
  sites. No implicit numeric coercion: every widening or
  narrowing requires explicit `as`.
- **Soundness.** Strict:
  - GC-backed memory; use-after-free is representationally
    impossible.
  - Exhaustive `match` — a missing variant is a hard error.
  - **Checked `as` casts** — restricted to a whitelist
    (numeric ↔ numeric, `bool`/`char` → integer, `u8` →
    `char`, same-type no-op). `String as i64` and every
    other non-primitive source is rejected (GT0005).
    Matches Rust's RFC 401 posture.
  - Runtime numeric semantics still follow Rust's rules
    (narrowing truncates, sign changes wrap) — that's
    inherent to `as`, not a soundness hole.
- **Concurrency.** First-class goroutines (`go expr`), typed
  channels (`channel::<T>()`), `select` for multiplexed receives,
  cooperative scheduler.
- **Closures.** `|x| expr` lambdas with capture by GC reference.
  Higher-order functions take `Fn(args) -> ret` parameters, which
  accept both bare `fn` items and capturing closures (env+code
  fat pointer under the hood). Bare `fn(args) -> ret` stays a
  raw code pointer for non-capturing items only.
- **Error handling.** `Result<T, E>` with `?` propagation, plus a
  `panic` primitive for unrecoverable failure.
- **Memory.** Managed by the stop-the-world mark-sweep GC. `&` and
  `&mut` express aliasing intent to the type checker without
  lifetime annotations.
- **Modules.** `use path::to::module`, flat file-based module
  hierarchy. `pub` visibility. Every project carries a
  `project.toml` manifest.
- **Stdlib.** `fmt`, `io`, `os`, `os::exec`, `os::signal`,
  `strings`, `strconv`, `collections`, `net`, `net::url`, `http`,
  `tls`, `encoding::json`, `sync`, `time`, `panic`, plus: `errors`,
  `flag`, `path`, `path::native`, `fs`, `bytes`, `bufio`, `context`,
  `slog`, `encoding::{base64,hex,binary}`, `compress::gzip`, `sort`,
  `utf8`, `math::rand`, `crypto::{rand,sha256,hmac,subtle}`,
  `runtime`, `testing`, **`regex`** (wraps the Rust `regex` crate:
  compile, is_match, find, captures, replace, split).
- **Tooling.** `gos` is the one-binary toolchain:
  `parse`, `check`, `run`, `build`, `fmt`, `doc`, `test`, `bench`,
  `lint`, `explain`, `watch`, `new`, `init`, `add`, `remove`,
  `tidy`, `fetch`, `vendor`, `lsp`. Bare `gos` drops into an
  interactive REPL.
- **Lints.** 50 built-in lints covering unused bindings, bool
  literals in conditions, self-comparisons, identity arithmetic,
  needless returns / bool / else, absurd ranges, redundant
  closures, and more. `gos lint --fix` auto-applies suggestions
  for a curated subset. Every lint has an `explain` page (`gos
  explain GL####`).
- **Doc-tests.** fenced code inside `//` doc comments
  is compiled and executed as a standalone program by `gos test`.
  Non-runnable fences (```text ```) are skipped.
- **Incremental cache.** `gos check` / `gos run` hash the source
  and reuse the parsed AST on re-invocation. Set
  `GOSSAMER_CACHE_TRACE=1` to see cache hits; `gos check
  --timings` prints per-stage wall-clock times. See
  [`docs_src/design/incremental.md`](docs_src/design/incremental.md)
  for the staged rollout to resolve / typecheck skipping.
- **LSP.** `gos lsp` speaks LSP 3.16 over stdio with
  `publishDiagnostics`, `hover`, `definition`, `completion`,
  `references`, and `rename` (with `prepareRename`). Editor
  integration is plug-and-play — see
  [`docs_src/toolchain.md`](docs_src/toolchain.md#editor-integration).

## Toolchain cheat-sheet

```sh
# Build the toolchain.
cargo build --workspace

# Create a new project.
./target/debug/gos new example.com/hello --path hello
cd hello

# Type-check, run, build.
gos check src/main.gos
gos run src/main.gos
gos build src/main.gos

# Lint, format, test.
gos lint .
gos fmt src/main.gos
gos test src/main.gos

# Drop into the REPL.
gos
```

## Repository layout

```
crates/               workspace crates (lex / parse / resolve / types / hir / mir /
                      driver / interp / gc / runtime / sched / std / pkg / lint /
                      diagnostics / codegen-cranelift / codegen-llvm / cli)
examples/             end-to-end .gos programs (hello_world, line_count,
                      web_server, kv_cache, json_pipeline, selfhost/*)
docs_src/             mkdocs source for the public docs site (markdown +
                      assets, including bundled design notes and the
                      AI skill card)
docs/                 built site (output of `mkdocs build`, served by
                      GitHub Pages from /docs on main)
references/           external reference material (e.g. the Go language
                      spec used as a comparison baseline)
```

## Build

```sh
cargo build --workspace
./target/debug/gos --version
```

## License

Licensed under Apache-2.0. See [`LICENSE`](LICENSE).
