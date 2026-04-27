# Gossamer

A garbage-collected, goroutine-powered, fast-compiling systems
language with Rust-flavoured syntax and Go-shaped runtime.

- Source on GitHub: [danpozmanter/gossamer](https://github.com/danpozmanter/gossamer)
- Language spec: [`SPEC.md`](https://github.com/danpozmanter/gossamer/blob/main/SPEC.md)
- Project style guide: [`GUIDELINES.md`](https://github.com/danpozmanter/gossamer/blob/main/GUIDELINES.md)
- Security policy: [`SECURITY.md`](https://github.com/danpozmanter/gossamer/blob/main/SECURITY.md)

**Status**: pre-1.0.0. Nothing here is stable.

## Hello, Gossamer

```gossamer
fn main() {
    println("hello, world")
}
```

## A taste of Gossamer

The forward-pipe operator (`|>`) threads a value through successive
calls left-to-right. `x |> f(a, b)` desugars to `f(a, b, x)`,
placing the piped value in the trailing positional slot. It
composes uniformly across functions, methods, and closures:

```gossamer
fn double(x: i64) -> i64 { x * 2 }
fn add(a: i64, b: i64) -> i64 { a + b }
fn clamp(lo: i64, hi: i64, x: i64) -> i64 {
    if x < lo { lo } else if x > hi { hi } else { x }
}

fn main() {
    let n = 3 |> double |> add(10) |> clamp(0, 100)
    println("arithmetic:", n)

    let words = "  Hello  World  "
        |> str::trim
        |> str::to_lowercase
        |> str::split(" ")
        |> iter::count

    println("words:", words)
}
```

Goroutines and channels use the same syntax:

```gossamer
fn main() {
    let (tx, rx) = channel::<i64>()
    go fn() { tx.send(40 |> add(2)) }()
    println("answer:", rx.recv())
}
```

## Why Gossamer

- **Fast compile times.** `gos check` and `gos build` (Cranelift
  debug) finish in single-digit milliseconds on small-to-medium
  projects — meaningfully faster than `go build` and an order of
  magnitude faster than warm `cargo build` for an equivalent
  source tree. `gos build --release` (LLVM, full optimisation)
  still beats warm `cargo build --release` on the same workload.
  `gos check` is comparable to `cargo check` on incremental
  edits and far ahead from a cold start, because the front-end
  is built around an incremental cache and the runtime ships as
  a pre-built static library — no per-build dependency-graph
  compile.
- **Two execution tiers from one binary.** The same source runs
  unchanged through `gos run` (interpreter + bytecode VM with
  optional Cranelift JIT) and `gos build` (native AOT — Cranelift
  for debug, LLVM `-O3` for release with per-function fallback to
  Cranelift). Iterate with the interpreter for instant feedback,
  ship the native binary for production performance — no
  separate toolchains, no dialect drift.
- **Go-style goroutines** (`go expr`) with typed channels and a
  cooperative scheduler.
- **Rust-style type system** — statically-typed, generics with
  trait bounds, pattern-matching, `Option<T>` / `Result<T, E>`.
- **Garbage-collected** — no lifetimes, no borrow checker surface.
  `&` and `&mut` still express aliasing intent. Capturing closures
  flow through `Fn(args) -> ret` parameters without `move` /
  trait-bound ceremony.
- **Safe Rust** implementation — no `unsafe` in the compiler or
  runtime. Every crate carries `#![forbid(unsafe_code)]`.
- **Batteries-included stdlib** — `fmt`, `io`, `os`, `http`,
  `encoding::json`, `sync`, `time`, plus a growing list of
  libraries for context, path/fs, bytes/bufio, URL, logging,
  encoding, crypto, regex, sort, CLI flags.

## Where to go next

- [Install](install.md) — build from source today, prebuilt
  binaries coming with the 1.0.0 release.
- [Running](running.md) — `gos` cheat-sheet.
- [Syntax](syntax.md) — grammar tour with worked examples.
- [Memory model](memory.md) — how values, references, and the
  GC fit together.
- [Writing libraries](libraries.md) — `project.toml`, module
  layout, publishing.
- [Standard library](stdlib.md) — module index.
- [Toolchain](toolchain.md) — every subcommand.
