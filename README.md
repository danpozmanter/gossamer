# Gossamer

[![CI](https://github.com/danpozmanter/gossamer/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/danpozmanter/gossamer/actions/workflows/ci.yml)

[Homepage and Docs](http://gossamer-lang.org/)

A language that balances developer experience, execution efficiency, and safety.

**Extensible in Rust.**

## Features inspired by multiple languages:

| Feature | Rust | Go | F# | Python | Elixir |
|---|---|---|---|---|---|
| Strong static type system | ✓ | ✓ | ✓ |  |  |
| Type inference | ✓ | ✓ | ✓ |  |  |
| Error handling via `?` with `Result` & `Option` | ✓ |  | ✓ |  |  |
| Exhaustive pattern matching | ✓ |  | ✓ |  | ✓ |
| Algebraic data types / discriminated unions | ✓ |  | ✓ |  | ✓ |
| (Local) Borrow checking | ✓ |  |  |  |  |
| No `null` by default | ✓ |  | ✓ |  | ✓ |
| Immutable by default | ✓ |  | ✓ |  | ✓ |
| Safe by default | ✓ |  | ✓ |  | ✓ |
| Optimized native release builds | ✓ | ✓ | ✓ |  |  |
| Garbage collected |  | ✓ | ✓ | ✓ | ✓ |
| Lightweight concurrency primitives |  | ✓ | ✓ |  | ✓ |
| Fault-isolated goroutines (a panic ends one task, not the process) | ✓ |  |  |  | ✓ |
| Crashed-task memory reclaimed automatically | ✓ |  |  |  | ✓ |
| Async-first ecosystem | ✓ | ✓ |  | ✓ | ✓ |
| Batteries-included standard library | ✓ | ✓ |  | ✓ | ✓ |
| Fast compilation |  | ✓ |  |  |  |
| Small portable binaries | ✓ | ✓ |  |  | ✓ |
| Pipe operator (`\|>`) |  |  | ✓ |  | ✓ |
| Functional-first programming style |  |  | ✓ |  | ✓ |
| Interpreted / scripting mode |  |  | ✓ | ✓ | ✓ |
| Interactive REPL |  |  | ✓ | ✓ | ✓ |

> **Block-scoped `defer`** (Swift / Zig style, not Go's function scope): a
> deferred expression runs when control leaves its enclosing `{ }` block —
> by fall-through, `return`, `break`, or `continue` — in LIFO order. A `defer`
> inside a loop body therefore runs at the end of *every* iteration. See
> [`examples/defer_cleanup.gos`](examples/defer_cleanup.gos).

> **`#[derive(...)]`** (Rust style): annotate a struct or enum with
> `#[derive(Clone, PartialEq, Eq, Default, Debug)]` and the matching methods are
> generated for you — `==` / `!=` compare field-by-field, `.clone()` copies,
> `Type::default()` builds a zero-valued instance, and `{:?}` prints
> `Name { field: value }`. Works with nested-struct fields, generic structs, and
> tuple/unit-variant enums; emitted as ordinary Gossamer code, so it runs
> identically on every tier. See [`examples/derive.gos`](examples/derive.gos).

## Details

- Language spec: [`SPEC.md`](SPEC.md)
- Project style guide: [`GUIDELINES.md`](GUIDELINES.md)
- AI skill card: [`SKILL.md`](SKILL.md) — drop this file into a model's context to teach it how to write idiomatic Gossamer (also embedded in `gos skill-prompt`).
- Editor integrations: [`danpozmanter/gossamer-editor-support`](https://github.com/danpozmanter/gossamer-editor-support) (VSCode, Vim, Neovim, Helix, Emacs, Sublime, Zed, plus a tree-sitter grammar)
- Contributing: [`CONTRIBUTING.md`](CONTRIBUTING.md)

Source files use the `.gos` extension.

The CLI is `gos`. 

Manifests live in `project.toml`.

Pre-stable. The compatibility policy the
project will adopt at its first stable tag is drafted at
[`docs_src/stability.md`](docs_src/stability.md) as a work in progress.

Until then, treat the public API as may-change-with-notice.

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

## Supported Platforms

The runtime's stackful goroutines (corosensei) need a per-arch
context-switch implementation. The current support matrix:

| OS       | Architecture                  | Status |
| -------- | ----------------------------- | ------ |
| Linux    | x86_64                        | First-class |
| Linux    | aarch64                       | First-class |
| Linux    | armv7 (32-bit ARM)            | Supported |
| macOS    | aarch64 (Apple Silicon)       | First-class |
| macOS    | x86_64 (Intel)                | Supported |
| Windows  | x86_64 (MSVC ABI)             | Supported |

Other targets compile but the goroutine scheduler will refuse to
start. Cross-compiling to the supported targets is wired up in
`gos build --target <triple>`.

## Status and Rough Roadmap

Examples all run via interpretation, compile in debug or release mode.

There are gaps to fill in the standard library, bugs and optimizations to find via real world usage.

This project is very early. Right now performance, resource usage, functionality, and productivity
all feel very promising. But do not trust this yet.

My main goals are:

* Making Gossamer reliable enough to run real production code, and trust.

* Optimizing Gossamer to be Go-grade or better for performance and resource usage.

* Building a reliable standard library to reduce the need to reach for third party libraries (using Golang as the gold standard, with small changes that feel right).

* Writing some ecosystem libraries for key functionality (gRPC, Postgres, etc) that shouldn't be in the standard library, but are necessary for real work.

* Ensuring the developer experience fits the broad goals I have for a language that can replace or reduce my use of Go, Rust, Python, and F#.

## Build

```sh
cargo build --workspace
./target/debug/gos --version
```

## License

Licensed under Apache-2.0. See [`LICENSE`](LICENSE).
