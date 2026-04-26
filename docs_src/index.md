# Gossamer

A garbage-collected, goroutine-powered, fast-compiling systems
language with Rust-flavoured syntax and Go-shaped runtime.

- Source on GitHub: [gossamer-lang/gossamer](https://github.com/gossamer-lang/gossamer)
- Language spec: [`SPEC.md`](https://github.com/gossamer-lang/gossamer/blob/main/SPEC.md)
- Project style guide: [`GUIDELINES.md`](https://github.com/gossamer-lang/gossamer/blob/main/GUIDELINES.md)
- Security policy: [`SECURITY.md`](https://github.com/gossamer-lang/gossamer/blob/main/SECURITY.md)

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
    let n = 3i64 |> double |> add(10i64) |> clamp(0i64, 100i64)
    println("arithmetic:", n)

    let words = "  Hello  World  ".to_string()
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
    go fn() { tx.send(40i64 |> add(2i64)) }()
    println("answer:", rx.recv())
}
```

## Why Gossamer

- **Fast front-end compile times.** `gos check` is built around an
  incremental cache so iterative editing stays interactive.
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
- **One-binary toolchain**: `gos parse / check / run / build /
  fmt / doc / test / bench / lint / explain / watch / new / init
  / add / remove / tidy / fetch / vendor`. Bare `gos` drops into
  an interactive REPL.

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
