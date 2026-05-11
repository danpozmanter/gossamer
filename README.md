# Gossamer

[![CI](https://github.com/danpozmanter/gossamer/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/danpozmanter/gossamer/actions/workflows/ci.yml)

[Homepage and Docs](http://gossamer-lang.org/)

A language that balances developer experience, execution efficiency, and safety.

**Extensible in Rust.**

### From Rust
* Surface syntax
* Strong static type system
* Error handling: Result & Option, and ? operator
* Exhaustive match
* Local borrow checking
* No null
* Immutable by default
* Safe by default
* Optimized "release" mode.

### From Go
* Garbage collected
* Goroutines
* Batteries included standard library
* Fast compilation
* Small portable release binaries

### From F#
* Forward pipe operator

### From Python
* Interpreted mode
* REPL with syntax highlighting

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

## Supported platforms

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

## Build

```sh
cargo build --workspace
./target/debug/gos --version
```

## License

Licensed under Apache-2.0. See [`LICENSE`](LICENSE).
