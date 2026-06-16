# Gossamer

A goroutine-powered, fast-compiling systems language with
Rust-flavoured syntax and a Go-shaped runtime. Memory is automatic
and deterministic - reference counting plus `arena { }` regions, no
tracing collector and no GC pauses.

- Source on GitHub: [danpozmanter/gossamer](https://github.com/danpozmanter/gossamer)
- Language spec: [`SPEC.md`](https://github.com/danpozmanter/gossamer/blob/main/SPEC.md)
- Project style guide: [`GUIDELINES.md`](https://github.com/danpozmanter/gossamer/blob/main/GUIDELINES.md)
- Security policy: [`SECURITY.md`](https://github.com/danpozmanter/gossamer/blob/main/SECURITY.md)

**Status**: pre-1.0.0 (currently 0.14.0). The surface is stable to
write against; the public API may still change before 1.0.

## Hello, Gossamer

```gossamer
fn main() {
    println!("hello, world")
}
```

## Hello, Goroutines and Channels

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

## Why Gossamer

- **Ergonomic** - Forward pipes, Rust-like error handling, minimal magic.
- **Efficient** - Gossamer runs with minimal impact on memory, and it runs fast.
- **Interpreted and Compiled** - Develop code quickly with a bytecode vm powered
interpreter and a REPL. Ship an optimized compiled single binary.
- **Go-style goroutines** - (`go expr`) with typed channels.
- **Go-style async** - Colorless functions and stackful coroutines.
- **Rust-style type system** - statically-typed, generics with
  trait bounds, pattern-matching, `Option<T>` / `Result<T, E>`.
- **Automatic memory, no GC pauses** - deterministic reference
counting plus `arena { }` regions reclaim values the moment the last
reference dies. No lifetimes, no borrow-checker surface, no tracing
collector.
- **Extensible in Rust** - Write libraries in a safe systems language.

## Where to go next

- [Install](install.md) - build from source today, prebuilt
  binaries coming with the 1.0.0 release.
- [Running](running.md) - `gos` cheat-sheet.
- [Syntax](syntax.md) - grammar tour with worked examples.
- [Memory model](memory.md) - how values, references, and
  automatic memory management fit together.
- [Writing libraries](libraries.md) - `project.toml`, module
  layout, publishing.
- [Standard library](stdlib.md) - module index.
- [Prelude](prelude.md) - everything available without imports.
- [Toolchain](toolchain.md) - every subcommand.

Coming from another language? Start with the migration guide for
[Rust](migration/rust.md), [Go](migration/go.md),
[Python](migration/python.md), [Kotlin](migration/kotlin.md), or
[F#](migration/fsharp.md) - each maps what you already know onto
Gossamer.
