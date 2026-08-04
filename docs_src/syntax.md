# Syntax tour

Gossamer's surface is Rust with two simplifications:

- No explicit lifetime annotations. References have implicit lexical
  lifetimes ending at the closing brace.
- Semicolons may replace newlines only between statements on the same line.
  They are separators, not trailing terminators.

See [`SPEC.md`](https://github.com/danpozmanter/gossamer/blob/main/SPEC.md)
for the full grammar and semantics.

Delimited lists use commas on one line and newlines across multiple lines.
This rule covers function arguments and parameters, closure parameters, struct
fields and literals, and enum variants and payload fields. Multiline trailing
commas are accepted for migration and removed by `gos fmt`.

Statement separators follow the same layout-first principle:

```gossamer
let width = 6; let height = 7; println(width * height)
```

The semicolons above replace newlines. A semicolon before a newline, `}`, or
the end of the file is invalid, so `let width = 6;` remains an error.

## Comments

Two forms, no others:

- `// ...` - line comment to end of line.
- `/* ... */` - block comment. Does **not** nest.

There is no separate `///` or `//!` doc-comment syntax. A run
of `//` lines immediately above an item (no blank line
between) is its documentation; a run at the top of a file is
the module's. Tooling reads these by position.

## Items

```gossamer
const PI: f64 = 3.14159
static MAX: u32 = 1024

type Id = i64            // transparent type alias

struct Point { x: f64, y: f64 }
struct Pair { first: i64, second: i64 }

enum Shape {
    Circle(f64),
    Rect { w: f64, h: f64 },
}

trait Area {
    fn area(&self) -> f64
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

## Top-level statements

The entry file may skip the `fn main` wrapper. Bare statements at file
scope become the body of an implicit `fn main()`; items declared
alongside them are hoisted out as usual:

```gossamer
println!("Hello World")
```

A `?` at the top level makes the implicit main return
`Result<(), errors::Error>`; set a process exit code with
`std::process::exit(n)`. See
[Top-level statements](language/top_level_statements.md) for the full
rules.

## Generic structs

A struct may carry one or more type parameters. The typechecker
infers each parameter from the field values at the construction
site - no turbofish annotation is needed:

```gossamer
struct Pair<A, B> { fst: A, snd: B }
struct Cell<T>    { value: T }

fn main() {
    // Parameters inferred: Pair<i64, String>
    let p = Pair { fst: 42, snd: "answer" }
    println!("{} = {}", p.fst, p.snd)   // 42 = answer

    // Same struct, different instantiation: Pair<i64, i64>
    let nums = Pair { fst: 10, snd: 32 }
    println!("{}", nums.fst + nums.snd)  // 42

    let c = Cell { value: 99 }
    println!("{}", c.value)              // 99
}
```

Field reads carry the per-instance concrete type. When two fields
share the same parameter (`Pair<i64, i64>`), arithmetic across
them typechecks directly - no extra annotation required.
Named struct literals use braces with keyed fields only. Tuple structs use
parenthesized construction.

Generic structs take multiple type parameters, and generic methods
work too: an `impl<T> Cell<T> { ... }` block specializes per
instantiation. Field access and methods run on all three tiers.

## Expressions

Everything is an expression. Blocks evaluate to their tail:

```gossamer
let max = if x > y { x } else { y }
let label = match status {
    200 => "ok"
    404 => "missing"
    _ => "other"
}
```

Match arms on separate lines do not require commas. Commas remain accepted,
and are required between expression-bodied arms written on the same line.

Integer range expressions are lazy `Iterator<T>` values, where explicitly
typed bounds preserve their integer type and otherwise default to `i64`. See
the [lazy iterator protocol](design/lazy_iterators.md) for ownership, adapters,
and terminal behavior. `lo..hi` excludes `hi`, while `lo..=hi` includes it. An
omitted lower bound starts at zero. An omitted upper bound is unbounded:
like Rust's `RangeFrom`, it
panics on overflow in debug builds, while release builds yield `i64::MAX`,
wrap to `i64::MIN`, and continue. The REPL prints open ranges without
realising them, such as `10..` or `..10`. Because `..=` is inclusive, it
always requires an upper bound; `10..=` is a parse error.

## Forward pipe (`|>`)

The forward-pipe operator threads a value through a chain of
calls. `x |> f` desugars to `f(x)`; `x |> f(a, b)` to
`f(a, b, x)` - the piped value lands in the last positional
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

Use one direct `_` argument when the value belongs in a different
position: `text |> strings::slice(_, 1, 3)` becomes
`strings::slice(text, 1, 3)`. A trailing `_` is accepted but is only
an explicit spelling of the default data-last rule. `_` can also be
the receiver in forms such as `text |> _.trim`; it may not be used
more than once in one pipe step.

## Pattern matching

- `_` - wildcard.
- `name` / `mut name` - bind.
- `Some(inner)` / `None` - variant destructure.
- `Point { x, y }` / `Point { x: a, y: b }` - struct destructure (and renamed).
- `(a, b)` - tuple destructure.
- `&value` / `&mut value` - shared or mutable reference destructure.
- `1..=5` / `1..5` - closed and exclusive range.
- `..=hi` / `..hi` / `lo..` - open-ended range (an open end covers up to
  the type maximum). `lo..=` is a parse error because `..=` requires an
  upper bound.
- `a | b` - or-pattern.
- `x @ 1..=3` - `@`-binding.
- `..` - rest.

Guards: `Some(n) if n > 0 => ...`

Range patterns are opaque to exhaustiveness, so a `_` arm is still
required. The struct, variant, tuple, and or-pattern forms also work in
irrefutable `let` bindings: `let Point { x, y } = p`, `let Shape::Pair(m,
n) = s`, `let (A(g, _) | B(g)) = v` (or-pattern alternatives must bind the
same names).

The left side of `=` is a pattern and the right side is an expression.
Consequently, `&mut place` on the right creates a mutable reference, while
`&mut pattern` on the left matches and removes a mutable-reference layer.
Reference mutability must match exactly. The inner binding receives an
independent value copy, including when the referent is an aggregate.

`mut name` is separate: it makes `name` reassignable. `let &mut value =
reference` does not make `value` reassignable, while `let &mut mut value =
reference` does. For a simple top-level copy, `let value = *reference` is often
clearer. Reference patterns are especially useful when nested, as in `let
(name, &mut count) = entry`.

## Conditions and let-chains

An `if` or `while` condition may chain clauses with `&&`, where each
clause is either `let PAT = expr` or a boolean. Earlier `let` bindings
are in scope for later clauses and the body:

```gossamer
if let Some(x) = a && let Some(y) = b && x > 0 {
    use(x + y)
}
while i < xs.len() && let n = xs[i] && n > 0 {
    sum += n
    i += 1
}
```

A `let` clause chain is `&&`-only: `||` cannot join `let` clauses
without parentheses.

## Loops

```gossamer
loop { ... break value }
while cond { ... }
for item in iter { ... }
```

`break value` returns a value from `loop`. `continue` jumps to
the top.

## Arrays, slices, Vec, and ranges

Gossamer follows Rust's sequence model:

- `[T; N]` is an owned fixed-size array. Use `#[a, b]` for an explicit fixed
  literal, or let an expected fixed type such as `[i64; 2]` shape `[a, b]`.
- `[T]` is unsized and is ordinarily used as `&[T]` or `&mut [T]`.
- `Vec<T>` is the only owned growable sequence. Bracket literals such as
  `[a, b]` create Vec values by default.
- `{key: value}` creates a `HashMap<K, V>`, and `#{a, b}` creates a
  `HashSet<T>` unless an expected `BTreeSet<T>` type shapes it.

See [Collection literals](collection_literals.md) for examples of Vec,
fixed-array, HashMap, HashSet, and BTreeSet construction.

`&[T; N]` and `&Vec<T>` coerce to `&[T]`; their mutable forms coerce to
`&mut [T]`. Arrays and slices support queries and non-resizing operations.
Mutable arrays and slices can reorder or replace existing elements with
`sort`, `reverse`, `swap`, and `fill`.
Only Vec supports `push`, `pop`, insertion, removal, truncation, extension,
reservation, and capacity queries. See [method support](method_support.md#vec)
for the exact surfaces.

A range is a lazy `Iterator<i64>` value: `(2..n)` is exclusive and `(1..=n)`
is inclusive. It can be iterated directly or stored and consumed later:

```gossamer
let first_three = 0..3
for i in first_three { println(i) }
```

The sequence combinators are methods on any Vec or range -
`filter`, `map`, `sum`, `count(pred)`, `any` / `all`, `find` /
`position`, `fold`, `min` / `max`, `take`, `step_by`, `join` - so a
query chains directly with no accumulator:

```gossamer
let odds_sq = (1..=9).filter(|n| n % 2 == 1).map(|n| n * n).sum()
let primes = (2..limit).filter(|k| sieve[k])
```

Range binds looser than arithmetic and tighter than `|>`, so
`i * i..n` reads `(i * i)..n`.

## Error handling

```gossamer
use std::{fs, io}

fn load(path: String) -> Result<String, io::Error> {
    let raw = fs::read_to_string(&path)?
    Ok(raw)
}
```

`?` propagates the `Err` variant. Wrap with
`std::errors::wrap(err, "while loading config")` for context.

## Arenas

```gossamer
arena {
    let tree = build_tree(16)
    total += check(&tree)
}
```

Everything allocated inside an `arena { }` block is bump-allocated
and freed wholesale when the block exits - on every exit path,
including early `return` and `?`. Allocation becomes a pointer bump;
reclamation is O(slabs) with no per-object teardown; small-enum nodes
drop their runtime header entirely (a two-pointer tree node is exactly
16 bytes). The contract: nothing allocated inside the block may be
referenced after it exits. See the
[memory model](memory.md#arenas-arena) for the full semantics.

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

`go expr` spawns a goroutine - a real stackful coroutine on the
M:N scheduler. Blocking primitives (channel ops, mutex contention,
`time::sleep`, network reads, filesystem syscalls) park the
goroutine, freeing the worker thread to run other goroutines.
Channels are typed: `channel()` / `channel(0)` is unbuffered,
`channel(n)` is bounded, and `channel::unbounded()` is the explicit
queue form. `select` multiplexes sends and receives.

Scheduling uses watchdog-requested cooperative safepoints. Park points and
function boundaries yield, and native loops poll every 1,024 taken backedges.
The watchdog requests coroutine suspension and may interrupt a blocking syscall
with `SIGURG` or a Windows APC. The VM yields its OS worker at the same
backedge interval but retains its separate bounded worker-pool limitation. See
[runtime design - Preemption](design/runtime.md#preemption).

## Closures and higher-order fns

Lambdas use `|param: T| body`; captures from the enclosing scope
work transparently (runtime-managed, no `move`).

Higher-order parameters distinguish two callable types:

| Type | Accepts | Representation |
|------|---------|----------------|
| `fn(args) -> ret` | raw pointer shape | raw code pointer |
| `Fn(args) -> ret` | bare items **and** capturing closures | env+code fat pointer |

```gossamer
fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }
fn add_one(y: i64) -> i64 { y + 1 }

fn main() {
    let scale = 10
    let scaled = |y: i64| scale * y    // captures `scale`
    println!("{}", apply(scaled, 5))   // 50
    println!("{}", apply(add_one, 41)) // 42 - bare fn coerces
}
```

The conversion at the call boundary is implicit. Single trait
variant - `FnMut` / `FnOnce` parse but lower to the same
`Fn(_)` shape (the borrow-style split Rust draws is unnecessary
with automatic memory management).

## Attributes

```gossamer
#[test]
fn add_adds() { ... }

#[bench]
fn bench_hot_path() { ... }

#[lint(allow(unused_variable))]
fn scratch() { let x = 1 }

#[cfg(test)]
mod point_tests { ... }

#[derive(Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Point { x: i64, y: i64 }
```

`#[derive(...)]` accepts exactly `Debug`, `Default`, `PartialEq`, `Eq`,
`PartialOrd`, and `Ord`. Any other name (`Clone`, `Hash`, `Copy`,
`Display`, `Serialize`, ...) is rejected with `GT0025`: copying,
comparison, hashing, and serialization are automatic and need no derive.

## Modules

```gossamer
use std::http
use std::http::{Handler, Request, Response}
use example.org/other::widget
```

Standard library modules require an explicit import. The import binds the
module's final path segment, or the requested alias, into the file:

```gossamer
use std::encoding::json
use std::fs as filesystem

let value = json::parse(text)?
let bytes = filesystem::read(path)?
```

Writing `json::parse(text)` without importing `std::encoding::json` is an
unresolved-name error. Prelude types, variants, macros, and functions listed
on the [Prelude page](prelude.md) remain available without imports.

A project's module tree is file-based: `src/foo.gos` becomes
`mod foo`, `src/bar/mod.gos` becomes `mod bar`.

## Numeric literals

Write bare literals by default. Inference picks the type from the
binding, the call site, or the return type; suffixes are reserved
for the rare standalone case with no contextual hint.

- `42` - plain int, inferred type. Defaults to `i64`.
- `42i32` / `42u64` - explicit width when context can't pin it.
- `0xff` / `0b1010` / `0o777` - bases.
- `1_000_000` - underscore separator.
- `1.0` - plain float, inferred type. Defaults to `f64`.
- `1.0f32` - explicit float width.

## String literals

- `"hello"` - ordinary double-quoted string. Spans multiple lines
  without extra syntax; embedded newlines are preserved.
- `"\n"` / `"\t"` / `"\\"` / `"\""` - standard escapes.
- `r"raw"` / `r#"with embedded "quotes""#` - raw strings.
- `b"bytes"` / `b'c'` - byte literals for binary protocols.

## Formatted output

Formatted output goes through six format macros. Each takes a
Rust-style format string with `{}` placeholders, plus named captures
`{ident}` for bindings in scope:

```gossamer
let name = "jane"
let age = 30
println!("hello, {name}! you are {age} years old.")
let greeting = format!("welcome, {}", name)
```

A named capture may walk a field path - `{account.balance}`, tuple
index `{t.0}`, nested `{o.inner.hits}` - with specs applying to the
path (`{account.balance:>8}`).

| Macro | Effect |
|-------|--------|
| `format!("…", a, b)` | Returns a `String`. |
| `println!("…", a, b)` | Writes to stdout + newline. |
| `print!("…", a, b)` | Writes to stdout, no newline. |
| `eprintln!("…", a, b)` | Writes to stderr + newline. |
| `eprint!("…", a, b)` | Writes to stderr, no newline. |
| `panic!("…", a, b)` | Unwinds with the rendered message. |

Alongside the format macros, a fixed set of desugar macros -
`matches!`, `todo!`, `unimplemented!`, `unreachable!`, `dbg!` - and the
build-time `regex!` / `sql!` / `codegen!` are built in. Any other
`name!(…)` is a parse error (`GP0001`): there is no user-defined macro
system. Compile-time metaprogramming instead goes through `comptime` - a
`comptime { ... }` block or `comptime fn` call is evaluated at compile
time and folded to a constant on every tier; see
[Comptime](language/comptime.md).

Format specs follow Rust's `{:spec}` grammar - width, alignment,
fill, zero-pad, radix, and precision (`{:>8}`, `{:08x}`, `{:^6}`,
`{:.2}`).

For the single-`String` output shape, `+` concatenates without
adding a separator:

```gossamer
let greeting = "hello, " + &name
```
