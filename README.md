# Gossamer

[![CI](https://github.com/danpozmanter/gossamer/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/danpozmanter/gossamer/actions/workflows/ci.yml)

[Homepage and Docs](http://gossamer-lang.org/)

## North Star

* Trustworthy (Stability, Security, Correctness)

* Ergonomic (Concise, Expressive)

* Performant (Solid Execution Speed, Efficient Resource Usage)

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for details on Github Issues, 
PRs, and our LLM policy.

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

My goal is for Gossamer to replace Go, Python, F#/C#, Kotlin/Java, and 
(some) Rust for most of my own projects and use cases.

## Features inspired by multiple languages:

| Feature                                         | Gossamer | Rust |  Go |  F# | Python | Elixir | Kotlin |
| ----------------------------------------------- | :------: | :--: | :-: | :-: | :----: | :----: | :----: |
| Strong static type system                       |    ✓     |   ✓  |  ✓  |  ✓  |        |        |    ✓   |
| Algebraic data types / discriminated unions     |    ✓     |   ✓  |     |  ✓  |        |        |        |
| Exhaustive pattern matching                     |    ✓     |   ✓  |     |  ✓  |        |    ✓   |    ✓   |
| Error handling via `?` with `Result` & `Option` |    ✓     |   ✓  |     |     |        |        |        |
| No `null` by default                            |    ✓     |   ✓  |     |  ✓  |        |    ✓   |    ✓   |
| Immutable by default                            |    ✓     |   ✓  |     |  ✓  |        |    ✓   |   ✓    |
| Reference mutability and escape checks          |    ✓     |   ✓  |     |     |        |        |        |
| Automatic memory management                     |    ✓     |      |  ✓  |  ✓  |    ✓   |    ✓   |    ✓   |
| Go style concurrency                            |    ✓     |      |  ✓  |     |        |        |        |
| Small portable binaries                         |    ✓     |   ✓  |  ✓  |     |        |    ✓   |        |
| Pipe operator (`\|>`)                           |    ✓     |      |     |  ✓  |        |    ✓   |        |
| Interpreted / scripting mode                    |    ✓     |      |     |  ✓  |    ✓   |    ✓   |    ✓   |
| Interactive REPL                                |    ✓     |      |     |  ✓  |    ✓   |    ✓   |    ✓   |
| Keyword arguments                               |    ✓     |      |     |  ✓  |    ✓   |   ✓    |    ✓   |
| Default argument values                         |    ✓     |      |     |  ✓  |    ✓   |   ✓    |    ✓   |

Gossamer's automatic memory management uses deterministic reference counting
and has no tracing collector. The compiled runtime can collect thread-local
cycles on demand; cross-goroutine cycles must be broken with `Weak<T>`, and the
bytecode VM currently treats `runtime::collect_cycles()` as a no-op. Cycle
collection is Experimental and is not a cross-tier compatibility promise.

Plus `arena { }` blocks, inspired by Zig: everything allocated inside
the block is bump-allocated and freed wholesale when the block exits -
pointer-bump allocation, O(slabs) reclamation, and headerless 16-byte
nodes for small enums. See the
[memory model](https://danpozmanter.github.io/gossamer/memory/) chapter.

**Not Transpiled**

Gossamer compiles directly to native, it does not transpile to Rust or Go.

**No Macros**

No user-defined macros. Metaprogramming is Zig-style `comptime`: code
runs during compilation and folds into the program, and a `for` loop
over `typeInfo::<T>()` reflection generates native per-field code.

**Gossamer is Extensible in Rust.**

Gossamer is built to extend simply via (synchronous) Rust.

## Features unique to Gossamer

Or at least - not a carbon copy by intent!

**Commas or Newlines**

For structs, enums, match branches, function arguments & parameters, 
use commas for single line, and newlines for multi-line. 
This gives a consistent and cleaner look.
(Optional - `gos fmt` will clean this up).

**Collection Literals**

Inspired by Clojure here (#{} for set):

| Collection | Empty | With Data |
|---|---|---|
| Fixed Array | [] | [1,2,3] |
| Vec | #[] | #[1,2,3] |
| Map | {} | {"one": 1, "two": 2, "three": 3} |
| Set | #{} | #{1,2,3} |
| Tuple | () | (1, "two", 3.0) |

**Distinct Types for Queue, Stack, MinHeap, MaxHeap**

Gossamer implements distinct types here instead of reusing existing structures.

Typically:

* MinHeap: MaxHeap with Reverse (or similar)
* Stack: Vec with specific method usage
* Queue: Deque with specific method usage

This enables having a stronger type contract as well as making it more convenient to write.

If I want to know an argument to a function only allows LIFO behavior - I'd use Stack over Vec.

If I want a MinHeap - I can create it and use push/pop without worrying about making
the number a negative, or using "Reverse" to wrap it.

Of course you can still use the other structures - but the recommended course of action
is to use the dedicated ones.

| Collection | Empty | With Data |
| MaxHeap | MaxHeap::new() | MaxHeap::from([1,2,3]) |
| MinHeap | MinHeap::new() | MinHeap::from([1,2,3]) |
| Queue | Queue::new() | Queue::from([1,2,3]) |
| Stack | Stack::new() | Stack::from([1,2,3]) |
| Deque | Deque::new() | Deque::from([1,2,3]) |

## Details

- Language spec: [`SPEC.md`](SPEC.md)
- Project style guide: [`GUIDELINES.md`](GUIDELINES.md)
- AI skill card: [`SKILL.md`](SKILL.md) - drop this file into a model's context to teach it how to write idiomatic Gossamer (also embedded in `gos skill-prompt`).
- Editor integrations: [`danpozmanter/gossamer-editor-support`](https://github.com/danpozmanter/gossamer-editor-support) (VSCode, Vim, Neovim, Helix, Emacs, Sublime, Zed, plus a tree-sitter grammar)

Source files use the `.gos` extension.

The CLI is `gos`. 

Manifests live in `project.toml`.

Pre-stable. `gos feature-status` distinguishes available Shipped surface from
compatibility-protected Stable surface. Until entries are explicitly promoted
to Stable, treat them as may-change-with-notice.

### Packages, Modules, and Visibility

A **package** is the unit of distribution: one `project.toml`, one project id,
the thing `gos add` pulls in. A **module** is a directory of source under
`src/` - `src/util/mod.gos` is module `util`. A module nested inside another is
a **module descendant**: `src/deep/nest/` is `deep::nest`, a descendant of
`deep`.

Visibility is defined against those three. An item with no annotation is
private to the module that declares it and to that module's descendants, as in
Rust. `pub(package)` widens it to every module of the declaring package and no
further. `pub` makes it part of the package's public API. Methods and struct
fields carry their own visibility, so a `pub` type can keep private helpers and
a private representation.

Full rules: [visibility](docs_src/language/visibility.md) and SPEC §6.3a.

### On Mutability and Ownership

The broad goal is be inspired by Rust, but not as strict.
Gossamer uses a conservative lexical borrow checker rather than Rust's
ownership and lifetime system. References have implicit lifetimes ending at
the closing brace, and safe Gossamer has no explicit lifetime annotations.
Bindings are immutable by default, and a function can mutate caller-owned data
only through a mutable reference.

## Parity and the REPL

Tier parity between interpreted Gossamer and compiled Gossamer is a primary
language goal.

This extends to the REPL as much as practical. Because top-level REPL bindings
share one persistent scope, a reference would otherwise protect its source for
the rest of the session. `%drop NAME` ends and removes that binding's lexical
lifetime while preserving completed mutations and later independent bindings.

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
        |> strings::to_lowercase
        |> strings::split_whitespace
        |> iter::count

    println!("words: {}", words)
}
```

Types define their own operators. `impl Add for T` gives `+` its
meaning, and the same shape covers `-`, `*`, `[]`, and the rest.
Structural `==` and `.clone()` are automatic - no derive needed - so a
custom operator is the part that is genuinely yours to write:

```gossamer
struct Vec2 { x: f64, y: f64 }

impl Add for Vec2 {
    fn add(self, o: Vec2) -> Vec2 { Vec2 { x: self.x + o.x, y: self.y + o.y } }
}

fn main() {
    let sum = Vec2 { x: 1.5, y: 2.0 } + Vec2 { x: 3.0, y: 4.0 }
    println!("({}, {})", sum.x, sum.y)   // (4.5, 6)
    println!("{}", sum == sum.clone())   // true
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

## REPL meta commands

The REPL starts with `gos <version> REPL [<architecture>-<os>]` and uses the
`>>>` prompt. Use `%help` to list commands: `%info`/`%i` searches public symbols
and shows item help, `%bindings`/`%b`, `%declarations`/`%d`,
and `%history`/`%h` inspect the session, `%reset`/`%r` clears it, and
`%quit`/`%q` exits. Up/down cycles history; Enter continues until braces close;
Ctrl-D also exits. Expression results print as plain values. Declaration and
binding confirmations are hidden unless the REPL is started with `-v`; listings
wrap to the terminal width.

## Toolchain commands

```sh
# Build the toolchain.
cargo build --workspace

# Create a new project.
./target/debug/gos new example.com/hello --path hello
cd hello

# Type-check, execute, build.
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

Sequence types follow Rust's model. `[T; N]` is an owned fixed-size array,
`[T]` is an unsized slice used behind `&` or `&mut`, and `Vec<T>` is the only
owned growable sequence. A bracket literal such as `[1, 2, 3]` creates a
`Vec` by default. Use `#[1, 2, 3]` when a fixed array is required explicitly,
or let an expected fixed type such as `[i64; 3]` shape a plain bracket literal.
Map literals use `{key: value}` and construct `Map` values. Set literals
use `#{value, ...}` and construct `Set` values, or `BTreeSet` values when
an expected `BTreeSet<T>` type is present.
References to arrays and Vec values coerce to slice references in the same
four shared and mutable forms as Rust. Arrays and slices expose the implemented
slice-method surface, while Vec additionally owns eager collection
combinators, resizing, and capacity operations. Mutable arrays and slices
support non-resizing mutation such as `sort`, `reverse`, `swap`, and `fill`.
`%i` shows these distinct type surfaces and `%e` filters them further by the
binding's writable capability.

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

The supported target contract is the executable matrix in
[`conformance/target_matrix.tsv`](conformance/target_matrix.tsv) and the
matching [supported-targets documentation](docs_src/supported_targets.md).
Tier 1 executes the bytecode VM, JIT-enabled VM, and LLVM AOT binaries on
native CI for Linux x86_64/aarch64, Apple Silicon macOS, and Windows x86_64.
Linux x86_64/aarch64 musl AOT output is Tier 2: it is built from supported
hosts, executed natively or under QEMU, and compared with the pure bytecode
VM. Intel macOS is artifact-only pending execution evidence; armv7, riscv64,
and wasm are not supported execution targets.

### Raspberry Pi

Raspberry Pi OS 64-bit (and any `aarch64` Linux) is first-class. Install
the `linux-aarch64` release, then `gos` works out of the box (the VM
and its in-process JIT are self-contained). To compile natively on the
Pi, also install system LLVM and a C compiler:

```sh
sudo apt-get install -y llvm clang
```

### Cross-compiling to a Raspberry Pi

Build a Pi binary from a Linux, macOS, or Windows desktop. The
musl-static target is the host-agnostic path (no target sysroot needed):

```sh
rustup target add aarch64-unknown-linux-musl
cargo build --release --target aarch64-unknown-linux-musl -p gossamer-runtime
gos build --release --target aarch64-unknown-linux-musl app.gos
# copy the static binary to the Pi and run it - no runtime deps
```

For a glibc (dynamic) Pi binary, target `aarch64-unknown-linux-gnu`; on a
Linux host install `gcc-aarch64-linux-gnu`, and on macOS/Windows supply an
aarch64 glibc sysroot via `GOS_CROSS_SYSROOT`. See SPEC §11.4 for the full
contract.

## Editor Support

Support for various editors (VS Code, Neovim, etc) [here](https://github.com/danpozmanter/gossamer-editor-support) - syntax and LSP support.
   
[Lite Anvil](https://github.com/danpozmanter/lite-anvil) supports Gossamer as a first class language (syntax & LSP).

## Status and Rough Roadmap

Examples run through the bytecode VM by default (with optional deferred JIT
tier-up) and compile in debug or release mode.

There are gaps to fill in the standard library, bugs and optimizations to find via real world usage.

This project is still early but starting to find its sea legs. Right now performance, resource usage, functionality, and productivity
all feel very promising. But do not trust this yet.

My main goals are:

* Making Gossamer reliable enough to run real production code, and trust.

* Optimizing Gossamer toward Go-grade performance and resource usage. Claims
  are limited to workloads recorded by the checked-in benchmark suite; broad
  language-level parity is a goal, not a current guarantee.

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
