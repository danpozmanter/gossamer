# Syntax tour

Gossamer's surface is Rust with two simplifications:

- No lifetime annotations. References express aliasing intent;
  the GC owns the memory.
- Semicolons are optional at statement boundaries.

See the full grammar in
[`grammar/`](https://github.com/danpozmanter/gossamer/tree/main/grammar)
once it is committed.

## Comments

Two forms, no others:

- `// ...` — line comment to end of line.
- `/* ... */` — block comment. Does **not** nest.

There is no separate `///` or `//!` doc-comment syntax. A run
of `//` lines immediately above an item (no blank line
between) is its documentation; a run at the top of a file is
the module's. Tooling reads these by position.

## Items

```gossamer
const PI: f64 = 3.14159
static MAX: u32 = 1024

struct Point { x: f64, y: f64 }
struct Pair(i64, i64)

enum Shape {
    Circle(f64),
    Rect { w: f64, h: f64 },
}

trait Area {
    fn area(&self) -> f64;
}

impl Area for Shape {
    fn area(&self) -> f64 {
        match self {
            Shape::Circle(r) => 3.14159 * r * r,
            Shape::Rect { w, h } => w * h,
        }
    }
}
```

## Expressions

Everything is an expression. Blocks evaluate to their tail:

```gossamer
let max = if x > y { x } else { y }
let label = match status {
    200 => "ok",
    404 => "missing",
    _ => "other",
}
```

## Forward pipe (`|>`)

The forward-pipe operator threads a value through a chain of
calls. `x |> f` desugars to `f(x)`; `x |> f(a, b)` to
`f(a, b, x)` — the piped value lands in the last positional
slot. Methods work the same way: `x |> recv.m(a)` becomes
`recv.m(a, x)`. `|>` is left-associative with very low
precedence, so `a |> f |> g` reads as `g(f(a))` with no
parentheses needed:

```gossamer
fn double(x: i64) -> i64 { x * 2 }
fn add(a: i64, b: i64) -> i64 { a + b }
fn clamp(lo: i64, hi: i64, x: i64) -> i64 {
    if x < lo { lo } else if x > hi { hi } else { x }
}

// Reads left-to-right instead of inside-out.
let n = 3 |> double |> add(10) |> clamp(0, 100)

// Equivalent nested form:
let same = clamp(0, 100, add(10, double(3)))
```

## Pattern matching

- `_` — wildcard.
- `name` / `mut name` — bind.
- `Some(inner)` / `None` — variant destructure.
- `Point { x, y }` — struct destructure.
- `(a, b)` — tuple destructure.
- `1..=5` — range.
- `a | b` — or-pattern.
- `x @ 1..=3` — `@`-binding.
- `..` — rest.

Guards: `Some(n) if n > 0 => ...`

## Loops

```gossamer
loop { ... break value }
while cond { ... }
for item in iter { ... }
```

`break value` returns a value from `loop`. `continue` jumps to
the top.

## Error handling

```gossamer
fn load(path: String) -> Result<String, io::Error> {
    let raw = os::read_file_to_string(&path)?
    Ok(raw)
}
```

`?` propagates the `Err` variant. Wrap with
`std::errors::wrap(err, "while loading config")` for context.

## Concurrency

```gossamer
let (tx, rx) = channel::<i64>()
go fn() { tx.send(42) }()
let n = rx.recv()

select {
    a = rx_a.recv() => handle_a(a),
    b = rx_b.recv() => handle_b(b),
    _ = time::after(5000) => timeout(),
}
```

`go expr` spawns a goroutine. Channels are typed and bounded;
`select` multiplexes receives.

## Closures and higher-order fns

Lambdas use `|param: T| body`; captures from the enclosing scope
work transparently (GC-managed, no `move`).

Higher-order parameters distinguish two callable types:

| Type | Accepts | Representation |
|------|---------|----------------|
| `fn(args) -> ret` | non-capturing items only | raw code pointer |
| `Fn(args) -> ret` | bare items **and** capturing closures | env+code fat pointer |

```gossamer
fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }

fn main() {
    let scale = 10
    let scaled = |y: i64| scale * y    // captures `scale`
    println!("{}", apply(scaled, 5))   // 50

    fn add_one(y: i64) -> i64 { y + 1 }
    println!("{}", apply(add_one, 41)) // 42 — bare fn coerces
}
```

The conversion at the call boundary is implicit. Single trait
variant — `FnMut` / `FnOnce` parse but lower to the same
`Fn(_)` shape (the borrow-style split Rust draws is unnecessary
in a fully GC'd world).

## Attributes

```gossamer
#[test]
fn add_adds() { ... }

#[bench]
fn bench_hot_path() { ... }

#[lint(allow(unused_variable))]
fn scratch() { let x = 1 }
```

## Modules

```gossamer
use std::http
use std::http::{Handler, Request, Response}
use example.org/other::widget
```

A project's module tree is file-based: `src/foo.gos` becomes
`mod foo`, `src/bar/mod.gos` becomes `mod bar`.

## Numeric literals

Write bare literals by default. Inference picks the type from the
binding, the call site, or the return type; suffixes are reserved
for the rare standalone case with no contextual hint.

- `42` — plain int, inferred type. Defaults to `i64`.
- `42i32` / `42u64` — explicit width when context can't pin it.
- `0xff` / `0b1010` / `0o777` — bases.
- `1_000_000` — underscore separator.
- `1.0` — plain float, inferred type. Defaults to `f64`.
- `1.0f32` — explicit float width.

## String literals

- `"hello"` — ordinary double-quoted string. Spans multiple lines
  without extra syntax; embedded newlines are preserved.
- `"\n"` / `"\t"` / `"\\"` / `"\""` — standard escapes.
- `r"raw"` / `r#"with embedded "quotes""#` — raw strings.
- `b"bytes"` / `b'c'` — byte literals for binary protocols.

## Formatted output

Gossamer has no macro system and no `!` syntax. Formatted output
goes through plain variadic builtins:

```gossamer
let name = "jane"
let age = 30
println("hello, ", name, "! you are ", age, " years old.")
let greeting = format("welcome, ", name)
```

Every builtin below stringifies each argument and joins them
with a single space:

| Builtin | Effect |
|---------|--------|
| `format(a, b, …)` | Returns a `String`. |
| `println(a, b, …)` | Writes to stdout + newline. |
| `print(a, b, …)` | Writes to stdout, no newline. |
| `eprintln(a, b, …)` | Writes to stderr + newline. |
| `eprint(a, b, …)` | Writes to stderr, no newline. |
| `panic(a, b, …)` | Unwinds with the rendered message. |

For the single-`String` output shape, `+` concatenates without
adding a separator:

```gossamer
let greeting = "hello, " + &name
```

Writing `name!(…)` is a hard parse error — the `!` suffix is
reserved for no purpose today.
