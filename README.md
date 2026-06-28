# Gossamer

[![CI](https://github.com/danpozmanter/gossamer/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/danpozmanter/gossamer/actions/workflows/ci.yml)

[Homepage and Docs](http://gossamer-lang.org/)

A language that balances developer experience, execution efficiency, and safety.

## Motivations

Why build Gossamer? Why use it?

I enjoy building web services and command line tools. 
I always have another idea I want to explore, another service I want to deploy, 
or another manual task I want to automate with a script.

I love the confidence that comes from Rust and F#: the feeling that if it
 compiles, it probably works. Algebraic data types, pattern matching, and 
 explicit error handling feel like a natural way to build correct and 
 maintainable software.

I also love having a REPL open or being able to iterate quickly on a script 
without waiting for a compile step.

Go, meanwhile, is an incredible tool for building and shipping software. 
It feels fast, minimal, and frictionless: a garbage-collected language with
 built-in concurrency and an extensive standard library.
 
### A Single Language?

What if one language could combine all of those ideas?

What if I could iterate quickly in a REPL or script, then compile the exact
 same program into an optimized standalone binary with no code changes?

What if that language could perform Python like while interpreted, 
but closer to Go when compiled?

I built Gossamer because I wanted that language for myself.

My goal is for Gossamer to replace Rust, Go, F#, Kotlin, and Python for most of
 my own projects and use cases.

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
| Automatic memory management |  | ✓ | ✓ | ✓ | ✓ |
| Lightweight concurrency primitives |  | ✓ | ✓ |  | ✓ |
| Fast compilation |  | ✓ |  |  |  |
| Small portable binaries | ✓ | ✓ |  |  | ✓ |
| Pipe operator (`\|>`) |  |  | ✓ |  | ✓ |
| Interpreted / scripting mode |  |  | ✓ | ✓ | ✓ |
| Interactive REPL |  |  | ✓ | ✓ | ✓ |

Gossamer's automatic memory management is **deterministic**: reference
counting reclaims a value the moment its last reference dies, an
automatic cycle collector handles reference cycles, and there is no
tracing collector. It is closely modeled on Swift's ARC, with the
addition that reference cycles are reclaimed for you instead of having
to be broken by hand.

Plus `arena { }` blocks, inspired by Zig: everything allocated inside
the block is bump-allocated and freed wholesale when the block exits -
pointer-bump allocation, O(slabs) reclamation, and headerless 16-byte
nodes for small enums. See the
[memory model](https://danpozmanter.github.io/gossamer/memory/) chapter.

**Not Transpiled**

Gossamer compiles directly to native, it does not transpile to Rust or Go.

**No Macros**

Metaprogramming (new in 0.21.0) is inspired by Zig's comptime.

**Gossamer is Extensible in Rust.**

Gossamer is built to extend simply via (synchronous) Rust.

## Details

- Language spec: [`SPEC.md`](SPEC.md)
- Project style guide: [`GUIDELINES.md`](GUIDELINES.md)
- AI skill card: [`SKILL.md`](SKILL.md) - drop this file into a model's context to teach it how to write idiomatic Gossamer (also embedded in `gos skill-prompt`).
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

For scripts and examples, the entry file may skip the `fn main` wrapper:
bare statements at file scope become the body of an implicit `fn main()`,
so this is a complete program:

```gossamer
println!("Hello World")
```

A top-level `?` makes the implicit main return `Result<(),
errors::Error>`; set a process exit code with `std::process::exit(n)`.

Gossamer leans on a forward-pipe operator (`|>`) so data flows
left-to-right. `x |> f(a, b)` desugars to
`f(a, b, x)`, and `|>` chains cleanly with methods, closures, and
plain functions:

```gossamer
use std::{iter, strings}

fn double(x: i64) -> i64 { x * 2 }
fn add(a: i64, b: i64) -> i64 { a + b }
fn clamp(lo: i64, hi: i64, x: i64) -> i64 {
    if x < lo { lo } else if x > hi { hi } else { x }
}

fn main() {
    // 3 -> double -> add 10 -> clamp to [0, 100]
    let n = 3 |> double |> add(10) |> clamp(0, 100)
    println!("arithmetic: {}", n)

    // Free functions pipe the same way.
    let words = "  Hello  World  "
        |> strings::to_lower
        |> strings::split_whitespace
        |> iter::count

    println!("words: {}", words)
}
```

A goroutine + channel example:

```gossamer
use std::sync::channel

fn add(a: i64, b: i64) -> i64 { a + b }

fn main() {
    let (tx, rx) = channel::<i64>()
    go fn() { tx.send(40 |> add(2)) }()
    if let Some(answer) = rx.recv() {
        println!("answer: {}", answer)
    }
}
```

Or spawn a goroutine and join its result - `Ok(value)`, or `Err(message)`
if it panicked:

```gossamer
fn add(a: i64, b: i64) -> i64 { a + b }

fn main() {
    let h = spawn(|| 40 |> add(2))
    match h.join() {
        Ok(v) => println!("answer: {}", v),
        Err(e) => println!("worker failed: {}", e),
    }
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

## Foreign Function Interface (FFI)

Gossamer can call native (Rust) code through the `[rust-bindings]`
section of `project.toml`. A Rust crate that depends on
`gossamer-binding` registers its entry points with `register_module!`,
and the toolchain compiles and links it into the produced binary (or
the interpreter) - the bound functions are then `use`-able from `.gos`
source like any other module:

```toml
# project.toml
[rust-bindings]
echo-binding = { path = "echo-binding" }
```

```gossamer
use echo::shout
fn main() { println!("{}", shout("hello")) }
```

The boundary uses the typed `gossamer-binding` ABI (integers, floats,
strings, tuples, vectors, `Option` / `Result`, opaque handles, byte
buffers, callbacks); a panic inside a binding is caught and surfaced as
a `Result::Err`. There is no source-level `extern "C"` item form - the
`extern` keyword is reserved (`GP0016`) and `[rust-bindings]` is the
single FFI surface. See [`SPEC.md` section 12](SPEC.md) and
[`example-external-libraries/`](example-external-libraries/) for two
end-to-end examples (a Gossamer-aware crate, and a plain published
crate wrapped thinly).

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

## Editor Support

Support for various editors (VS Code, Neovim, etc) [here](https://github.com/danpozmanter/gossamer-editor-support) - syntax and LSP support.
   
[Lite Anvil](https://github.com/danpozmanter/lite-anvil) supports Gossamer as a first class language (syntax & LSP).

## Status and Rough Roadmap

Examples all run via interpretation, compile in debug or release mode.

There are gaps to fill in the standard library, bugs and optimizations to find via real world usage.

This project is still early but starting to find it's sea legs. Right now performance, resource usage, functionality, and productivity
all feel very promising. But do not trust this yet.

My main goals are:

* Making Gossamer reliable enough to run real production code, and trust.

* Optimizing Gossamer to be Go-grade or better for performance and resource usage. (This feels close!)

* Building a reliable standard library to reduce the need to reach for third party libraries (using Golang as the gold standard, with small changes that feel right).

* Writing some ecosystem libraries for key functionality (gRPC, Postgres, etc) that shouldn't be in the standard library, but are necessary for real work. (Very early).

* Ensuring the developer experience fits the broad goals I have for a language that can replace or reduce my use of Go, Rust, Python, and F#.

## Build

```sh
cargo build --workspace
./target/debug/gos --version
```

## License

Licensed under Apache-2.0. See [`LICENSE`](LICENSE).
