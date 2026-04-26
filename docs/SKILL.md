# Gossamer — Skill Card

Drop this file into a model's context to teach it how to write
idiomatic Gossamer. Self-contained. Covers: what Gossamer is,
surface syntax, forward-pipe style, the `gos` toolchain, error
handling, concurrency, stdlib surface, and how to test. No prior
context assumed.

---

## 1. What Gossamer is

A garbage-collected, goroutine-powered, fast-compiling systems
language. Syntax is Rust-flavoured without lifetimes or a borrow
checker. Runtime is Go-shaped: goroutines, channels, GC. Source
files end in `.gos`. The toolchain binary is `gos`. Every project
ships a `project.toml` manifest.

Status: pre-1.0.0. Surface is stable enough to write against;
runtime and native codegen are partially wired — see "current
gaps" at the bottom.

## 2. Idioms at a glance

Prefer these shapes when writing Gossamer:

- **Left-to-right dataflow with `|>`.** Chain calls with the
  forward-pipe operator instead of nesting.
- **Plain functions for free-standing logic.** Reach for
  `impl` only when state is genuinely tied to a type.
- **`Result<T, E>` + `?` for fallibility.** Panic only for
  invariant violations.
- **Exhaustive `match`.** Leave no `_ =>` arm unless every
  unmatched case genuinely means the same thing.
- **Goroutines + channels for async work.** Share by
  communicating; reach for `sync::Mutex` only when
  shared-memory is the simpler model.
- **Explicit numeric widths.** `0i64` not `0` when the context
  doesn't pin the type.
- **Macros only for formatted output.** `println!`,
  `format!`, `print!`, `eprintln!`, `eprint!`, `panic!` are
  the six macro entries — no others exist.

## 3. The `|>` forward-pipe operator

Prefer `|>` over nested calls whenever a value flows through
two or more transformations.

- `x |> f` desugars to `f(x)`.
- `x |> f(a, b)` desugars to `f(a, b, x)` — the piped value
  lands in the **last positional slot**.
- `x |> recv.m(a)` becomes `recv.m(a, x)` — methods compose
  the same way.
- `|>` is left-associative with very low precedence, so
  `a |> f |> g` reads as `g(f(a))` without parentheses.

```gossamer
fn double(x: i64) -> i64 { x * 2 }
fn add(a: i64, b: i64) -> i64 { a + b }
fn clamp(lo: i64, hi: i64, x: i64) -> i64 {
    if x < lo { lo } else if x > hi { hi } else { x }
}

// Preferred — reads top-down.
let n = 3i64 |> double |> add(10i64) |> clamp(0i64, 100i64)

// Discouraged — the same meaning, but the eye has to unwind.
let same = clamp(0i64, 100i64, add(10i64, double(3i64)))
```

When a step is a closure, write it inline — `|>` still threads
the value into the last slot:

```gossamer
let result = input
    |> parse_header
    |> validate
    |> |row| { row.body }
    |> write_out
```

## 4. Cheat sheet

```gossamer
use std::io

const PI: f64 = 3.14159
static MAX_RETRIES: u32 = 3

struct Point { x: f64, y: f64 }
struct Pair(i64, i64)
enum Shape { Circle(f64), Rect { w: f64, h: f64 } }

trait Area { fn area(&self) -> f64; }

impl Area for Shape {
    fn area(&self) -> f64 {
        match self {
            Shape::Circle(r) => 3.14159 * r * r,
            Shape::Rect { w, h } => w * h,
        }
    }
}

fn main() {
    let mut total = 0i64
    for n in [1i64, 2i64, 3i64].iter() {
        total = total + *n
    }
    println!("total: {}", total)
}
```

## 5. Grammar essentials

- **Comments**: `//` single-line and `/* ... */` block
  (block comments nest). `///` on `pub` items is a doc
  comment that `gos doc` renders and `gos test` runs.
- **Semicolons** are optional at statement boundaries; one
  statement per line.
- **Expressions-as-statements.** `if`, `match`, `loop`, and
  block expressions all yield values.
- **Bindings.** `let name = expr`, `let mut name = expr`,
  `let Point { x, y } = p` (destructure), `let (a, b) = pair`.
- **References.** `&x` read-shared, `&mut x` exclusive write.
  Aliasing intent only; the GC owns memory. **No lifetimes,
  no borrow checker.**
- **Types.** `bool`, `char`, `i8..i128`, `u8..u128`, `isize`,
  `usize`, `f32`, `f64`, `String`, `[T]`, `(A, B)`,
  `Option<T>`, `Result<T, E>`, `&T`, `&mut T`, user types.
- **Integer literals** take a suffix: `1i64`, `255u8`, `0usize`.
  Unsuffixed literals default to `i64`; be explicit when the
  context doesn't already pin the type.
- **Casts.** `x as i32` — whitelist-checked (numeric ↔ numeric,
  `bool` / `char` → integer, `u8` → `char`, same-type no-op).
  Every other `as` shape is a hard error (GT0005).
- **Patterns.** Wildcard `_`, literals, `name`, `mut name`,
  `Variant(…)`, `Struct { … }`, tuples `(a, b)`, ranges
  `1..=5`, or-patterns `a | b`, `@`-bindings `x @ 1..=3`,
  rest `..`. Guards: `Some(n) if n > 0 => …`.

## 6. Formatted output (the only macros)

Gossamer has exactly six macros, all format-shaped. Every other
`name!(…)` is a parse error.

| Macro | Returns | Destination |
|-------|---------|-------------|
| `format!("…", a, b)` | `String` | — |
| `println!("…", a, b)` | `()` | stdout + newline |
| `print!("…", a, b)` | `()` | stdout, no newline |
| `eprintln!("…", a, b)` | `()` | stderr + newline |
| `eprint!("…", a, b)` | `()` | stderr, no newline |
| `panic!("…", a, b)` | `!` | unwinds with the rendered message |

Each macro supports Rust-style `{}` placeholders and
named-capture via `{ident}` for bindings in scope:

```gossamer
let name = "jane"
println!("hello, {name}!")
println!("value: {} / {}", answer, total)
```

The six macros lower to one allocation through the internal
`__concat` builtin. For building a single `String` piece-by-
piece, `+` concatenates without a separator:

```gossamer
let greeting = "hello, ".to_string() + &name
```

## 7. Error handling

Fallible functions return `Result<T, E>`. Propagate with `?` and
build / wrap / inspect errors through `std::errors`:

```gossamer
use std::errors
use std::os

fn load_config(path: &String) -> Result<String, errors::Error> {
    os::read_file_to_string(path)
        .map_err(|e| errors::wrap(e, format!("reading {}", path)))
}
```

- `errors::new(msg)` — build a free-standing error.
- `errors::wrap(cause, msg)` — add a higher-level message.
- `errors::is(err, needle)` — walk the cause chain.
- `errors::chain(err)` — iterate the cause chain.
- `errors::join([err, err])` — combine several into one.

Panics abort the current goroutine (and, for now, usually the
process). Reserve them for invariant violations, not
recoverable failure.

## 8. Concurrency

Goroutines via `go expr`. Typed channels via
`std::sync::channel()`. `select { }` multiplexes receives and
sends:

```gossamer
use std::sync::channel
use std::time

fn main() {
    let pair = channel()
    let tx = pair.0
    let rx = pair.1

    go tx.send(10i64)
    go tx.send(20i64)
    go tx.send(30i64)

    time::sleep(50u64)

    let mut total = 0i64
    loop {
        match rx.recv() {
            Some(v) => total = total + v,
            None => break,
        }
    }
    println!("total: {}", total)
}
```

`select { }` multiplexes:

```gossamer
select {
    x = rx_a.recv() => handle_a(x),
    y = rx_b.recv() => handle_b(y),
    default => do_something_else(),
}
```

- Prefer channels for coordination; reach for `sync::Mutex`
  only when shared-memory updates are the simpler shape.
- `go` takes a full expression — usually a function or method
  call. Closures work (`go || { ... }()`) but a named helper
  is easier to read and test.
- The current scheduler is cooperative and early-stage. Don't
  assume blocking semantics; pair producers and consumers
  with a short `time::sleep` or a `select { default => … }`
  arm when you need to drain deterministically.

## 9. Data structures

- `[T]` — growable array. Literal: `[1i64, 2i64, 3i64]`.
- `(A, B, …)` — tuple. Field access via `.0`, `.1`, ….
- `struct Foo { x, y }` / `struct Pair(A, B)` — GC-managed
  value types.
- `enum E { A, B(Payload) }` — sum types, pattern-matched
  exhaustively.
- `Option<T>` — `Some(T)` / `None`.
- `Result<T, E>` — `Ok(T)` / `Err(E)`.
- `std::collections::{Vec, HashMap, HashSet, BTreeMap}` —
  the richer containers; dispatch is wiring-dependent today,
  so verify with a small test if unsure.

## 10. The `gos` toolchain

Every subcommand takes a `.gos` file or a project directory.
Bare `gos` drops into the REPL.

| Command | Purpose |
|---------|---------|
| `gos check FILE` | Parse + resolve + typecheck + exhaustiveness. |
| `gos run FILE` | Register-based VM by default; falls back to the tree-walker when the VM hits something it doesn't yet support. |
| `gos run --vm FILE` | Require the VM (no fallback). |
| `gos run --tree-walker FILE` | Use the tree-walker directly. |
| `gos build FILE` | Native build via Cranelift + system `cc`. |
| `gos test PATH` | Discover and run `#[test]` functions. |
| `gos bench PATH` | Discover and time `#[bench]` functions. |
| `gos fmt [--check] FILE` | Rewrite canonically. |
| `gos doc FILE` | Print item listing + doc comments. |
| `gos lint [--deny-warnings] PATH` | Run the lint suite. |
| `gos explain CODE` | Long-form rationale for a diagnostic code. |
| `gos watch --command CMD PATH` | Re-run on file change. |
| `gos new ID --path DIR` | Scaffold a project. |
| `gos add SPEC` / `remove ID` / `tidy` / `fetch` / `vendor` | Package manager. |

## 11. Writing tests

Unit tests live inside the file they cover, under
`#[cfg(test)] mod tests { … }`. Integration tests live under
`tests/` in a project.

```gossamer
pub fn add(a: i64, b: i64) -> i64 { a + b }

#[cfg(test)]
mod tests {
    #[test]
    fn add_adds() {
        let total = super::add(2i64, 3i64)
        assert(total == 5i64)
    }
}
```

Doc-tests: fenced code inside `///` doc comments is compiled
and executed by `gos test`. Mark non-runnable fences as
` ```text `.

## 12. Standard library surface

- `std::fmt` — `Display`, `Debug`.
- `std::io` — `Read`, `Write`, buffered wrappers, `stdin` / `stdout`.
- `std::os` — process environment, argv, filesystem primitives.
- `std::strings` / `std::strconv` — string and numeric helpers.
- `std::collections` — `Vec`, `HashMap`, `HashSet`, `BTreeMap`.
- `std::net` — `TcpListener`, `TcpStream`, `UdpSocket`, DNS.
- `std::net::url` — URL parse + render + escape.
- `std::http` — `Method`, `StatusCode`, `Headers`, `Request`,
  `Response`, `Handler`, `serve`.
- `std::encoding::{json, base64, hex, binary}`.
- `std::sync` — `Mutex`, `RwLock`, atomics, `channel`, `Once`.
- `std::time` — `Instant`, `Duration`, `sleep`, `now`.
- `std::context` — cancellation, deadlines, `Context::background()`.
- `std::bytes` / `std::bufio` — binary buffers and buffered IO.
- `std::errors` — wrap / chain / join.
- `std::flag` — CLI flag parser.
- `std::sort` / `std::utf8` / `std::path` / `std::fs`.
- `std::math::rand` — deterministic RNG.
- `std::crypto::{rand, sha256, hmac, subtle}` — narrow, audited.
- `std::slog` — structured logging.
- `std::runtime` — scheduler + GC knobs.
- `std::testing` — `check`, `check_eq`, `Runner`, `check_ok`.
- `std::regex` — wraps the Rust `regex` crate.

Reality check: many modules exist in the manifest with
partial implementations. Trust examples in the repo; write
a small test when unsure.

## 13. Project layout

```
project.toml       # manifest: [project], [dependencies], [registries]
src/
├── main.gos       # binary entry
├── lib.gos        # library root (optional)
└── subdir/
    └── mod.gos    # module `subdir`
tests/             # integration tests
```

`project.toml`:

```toml
[project]
id      = "example.com/widget"
version = "0.1.0"
authors = ["Jane Roe <jane@example.com>"]
license = "Apache-2.0"

[dependencies]
"example.org/lib" = "1.2.3"
```

## 14. Worked examples

### CLI flags

```gossamer
use std::flag
use std::os

fn main() -> Result<(), flag::Error> {
    let mut fs = flag::Set::new("myapp")
    let port = fs.uint("port", 8080u64, "listen port")
    let verbose = fs.bool("verbose", false, "chatty output")
    let _ = fs.parse(os::args())?

    if *verbose {
        println!("starting on port {}", *port)
    }
    Ok(())
}
```

### HTTP server

```gossamer
use std::http

struct App { }

impl http::Handler for App {
    fn serve(&self, r: http::Request) -> Result<http::Response, http::Error> {
        Ok(http::Response::text(200, format!("hi from {}", r.path)))
    }
}

fn main() -> Result<(), http::Error> {
    let app = App { }
    println!("listening on 0.0.0.0:8080")
    http::serve("0.0.0.0:8080".to_string(), app)
}
```

## 15. Current gaps (pre-1.0.0)

- Integer inference is shallow. Prefer explicit suffixes.
- `+` on `String` copies; for heavy assembly use
  `std::bytes::Builder` or a `mut String` with `+=`.
- Method dispatch is name-global in places. Qualified path
  calls (`Point::origin()`) always work; method-style may
  collide across types until the resolver tightens.
- The scheduler is cooperative and unbuffered today.
  Channels work under `gos run`; `gos build` for programs
  that create channels is not yet wired — it will bail with
  a clear message. `go` spawn by itself builds natively.
- `os::args()` can return empty under some codegen paths —
  prefer `std::flag` with explicit defaults.

## 16. Style rules

- **No emojis.** Source, comments, commits, docs — all plain.
- **No TODO / FIXME** committed; open an issue.
- **Doc every `pub` item** with a single-line `///`; don't
  narrate self-evident code.
- **Pipe aggressively** — if a value flows through more
  than one call, use `|>`.
- **One statement per line;** omit semicolons.
- **Derive `Debug`, `Clone`, `PartialEq`** when cheap and
  meaningful; derive `Default` for zero-valued types.

## 17. Where to read more

- Language spec: `SPEC.md` (repo root).
- Project style guide: `GUIDELINES.md` (repo root).
- Rendered docs: `docs_src/` (source) → `site/` (built).
- Examples: `examples/` — start with `hello_world.gos`,
  `function_piping.gos`, `go_spawn.gos`, `concurrency.gos`.

## 18. When in doubt

Run it. `gos check` gives rustc-class diagnostics with source
excerpts and did-you-mean suggestions. `gos explain <CODE>`
expands any diagnostic code. The toolchain is your first
debugger.
