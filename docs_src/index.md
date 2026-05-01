# Gossamer

A garbage-collected, goroutine-powered, fast-compiling systems
language with Rust-flavoured syntax and Go-shaped runtime.

- Source on GitHub: [danpozmanter/gossamer](https://github.com/danpozmanter/gossamer)
- Language spec: [`SPEC.md`](https://github.com/danpozmanter/gossamer/blob/main/SPEC.md)
- Project style guide: [`GUIDELINES.md`](https://github.com/danpozmanter/gossamer/blob/main/GUIDELINES.md)
- Security policy: [`SECURITY.md`](https://github.com/danpozmanter/gossamer/blob/main/SECURITY.md)

**Status**: pre-stable (version 0.0.0). The compatibility
policy that takes effect at the first stable tag is drafted at
[`stability.md`](stability.md); until then, the public API may
change with notice in release notes.

## Hello, Gossamer

```gossamer
fn main() {
    println("hello, world")
}
```

## Hello, Goroutines and Channels

```gossamer
fn main() {
    let (tx, rx) = channel::<i64>()
    go fn() { tx.send(40 |> add(2)) }()
    println("answer:", rx.recv())
}
```

## Why Gossamer

- **Ergnomic** - Forward pipes, Rust like error handling, minimal magic.
- **Efficient** - Gossamer runs with minimal impact on memory, and it runs fast.
- **Interpreted and Compiled** - Develop code quickly with a bytecode vm powered
interpreter and a REPL. Ship an optimized compiled single binary.
- **Go-style goroutines** - (`go expr`) with typed channels.
- **Go-style async** - Colorless functions and stackful coroutines.
- **Rust-style type system** - statically-typed, generics with
  trait bounds, pattern-matching, `Option<T>` / `Result<T, E>`.
- **Garbage-collected** - no lifetimes, no borrow checker surface.
- **Estensible in Rust** - Write libraries in a safe systems language

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
