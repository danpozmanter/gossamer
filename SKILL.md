# Gossamer - Skill Card

Drop this file into a model's context to teach it how to write
idiomatic Gossamer. Self-contained. Covers: what Gossamer is,
surface syntax, forward-pipe style, the `gos` toolchain, error
handling, concurrency, stdlib surface, and how to test. No prior
context assumed.

---

## 1. What Gossamer is

A goroutine-powered, fast-compiling language with
automatic memory management (deterministic reference counting
plus `arena { }` regions - no borrow checker, no lifetimes, no
tracing-GC pauses). Syntax is Rust-flavoured. Runtime is
Go-shaped: goroutines, channels. Source files end in `.gos`. The
toolchain binary is `gos`. Every project ships a `project.toml`
manifest.

Status: pre-1.0.0. The surface is stable to write against, and
features ship across all three tiers (bytecode VM, in-process JIT,
LLVM AOT) - see "current gaps" at the bottom.

## 2. Idioms at a glance

Write **clear, low-complexity, concise code.** Names earn their
length; helpers earn their existence. If a line reads cleanly the
first time through, leave it alone.

- **Default to immutable.** `let x = …` first; `let mut` only when
  a binding genuinely changes after construction. Build new values
  with expressions (`if`, `match`, `loop … break v`, folds) and
  return them; callers shadow.
- **Compound-assign accumulators.** `+= -= *= /= %= &= |= ^= <<= >>=`.
  Write `x += 1`, never `x = x + 1`.
- **`if let` / `while let` for `Option` and single-variant matches.**
  `if let Some(n) = m.get(&k) { use(n) }`. `while let Some(v) =
  rx.recv()` is the canonical channel drain.
- **Tuple destructuring at every binding site.** `let (a, b) =
  pair`, `for (k, v) in m.iter()`, `let (tx, rx) = channel()`.
- **`for x in xs` over collections - no `.iter()`, no `*x`.** The
  binding is the value for `Copy` types, a borrow otherwise.
- **Bare integer indices - no `as usize`.** `arr[i]` works for
  `i: i64`. For scalar element types an index outside `[0, len)`
  yields the element's zero value rather than panicking (identical on
  every tier) - so guard with `len()` when absence must differ from
  zero. For aggregate elements (`Vec<Struct>`, `v[i].field`) an
  out-of-range access panics with `index out of bounds` on every
  tier rather than fabricating a zero aggregate.
- **`arr.swap(i, j)`** over the manual three-line temp dance.
- **`m.inc(k)` / `m.inc(k, by)`** for counters; `m.or_insert(k,
  default)` for get-or-fill.
- **Recursive enums work directly.** `enum List { Cons(i64,
  Box<List>), Nil }`. `Box` / `Arc` / `Rc` are transparent - every
  variant payload is heap-shared; the bare `Cons(i64, List)` form
  works too.
- **Structs and enums compare by value - no derive.** `==`, `!=`,
  `<`, `<=`, `>`, `>=` work on any struct / enum whose fields are all
  comparable (scalars, `String`, nested comparable types), exactly as
  they do on tuples. Ordering is lexicographic by declaration order
  (structs) or variant rank then payload (enums); a user `impl` of
  `eq` / `cmp` overrides the synthesized one for custom ordering.
  Arrays, `Vec`, and tuples also compare structurally (element-wise),
  not by identity: `[1, 2, 3] == [1, 2, 3]` is `true`.
- **Labeled loops** for breaking/continuing an outer loop from a
  nested one: `'outer: for i in 0..5 { for j in 0..5 { if j == 2 {
  continue 'outer } if i == 3 { break 'outer } } }`.
- **`#[derive(Debug, Default, PartialEq, Eq, PartialOrd, Ord)]`** are the
  derivable traits, synthesized as real source so `{:?}`,
  `Type::default()`, and the comparisons work on every tier. Enums
  derive for tuple, unit, and struct-payload (`Rect { w, h }`)
  variants; `#[default]` picks the `Default` variant. `Clone`, `Hash`,
  `Copy`, `Display`, `Serialize`, and the operator / conversion traits
  are **not** derivable (`GT0025`): copy (`let b = a`, `a.clone()`),
  hashing, comparison, and serde are automatic, and conversions /
  operators are written `impl Trait for T`: `From` / `TryFrom`, and the
  overloadable operators `Add` `Sub` `Mul` `Div` `Rem` (`%`) `Neg`
  (unary `-`) `Index` (`a[i]`) `BitOr` `BitAnd` `BitXor` `Shl` `Shr`.
- **`x.into()` / `x.try_into()`** convert to the inferred target type
  `B` via its `B::from(x)` / `B::try_from(x)` impl (target taken from a
  `let B` / `B` parameter / return).
- **`defer expr` for cleanup.** Runs when control leaves the
  enclosing `{ }` block by any path (fall-through, `return`,
  `break`, `continue`), LIFO order. In a loop body it runs each
  iteration.
- **`let PAT = expr else { … }`** for refutable-let-or-diverge; the
  else block must `return` / `break` / `continue` / `panic!`.
- **Left-to-right dataflow with `|>`.** Chain instead of nesting.
- **Plain functions for free-standing logic;** reach for `impl`
  only when state is genuinely tied to a type.
- **`Result<T, E>` + `?` for fallibility.** Panic only for
  invariant violations.
- **Exhaustive `match`.** No `_ =>` unless every unmatched case
  genuinely means the same thing.
- **Goroutines + channels for async work.** Share by communicating;
  reach for `sync::Mutex` only when shared memory is simpler.
- **`arena { ... }` for object graphs that die together.**
  Everything allocated inside is bump-allocated and freed wholesale
  at every exit path. Contract: nothing allocated inside may be
  referenced after the block - compute scalar/string summaries
  inside, keep survivors outside. Statement position only; nests.
  This is statically enforced, not just a convention: assigning an
  arena-allocated value to a binding outside the block is a compile-time
  `gos check` error (`GM0003`).
- **Bare numeric literals - always.** `0`, `200`, `1.5`, not
  `0i64` / `1.5f64`. Inference picks the type from binding, call
  site, or return type. Suffix only when a literal stands alone
  with no contextual hint. Same for indices: `arr[0]`.
- **String literals are already `String`.** Don't write
  `"foo".to_string()`. `&"foo"` borrows where `&String` / `&str`
  is expected.
- **Small fixed macro set.** Formatted output (`println!`, `format!`,
  `print!`, `eprintln!`, `eprint!`, `panic!`); `matches!(e, pat)`,
  `todo!`, `unimplemented!`, `unreachable!`, `dbg!`; and the build-time
  `regex!` / `sql!` / `codegen!`. Every other `name!(...)` is a parse
  error - there are no user-defined macros.

### Immutability default - concrete examples

```gossamer
// `if` / `match` are expressions - bind their result.
let label = if n < 0 { "negative" } else { "non-negative" }
let label = match shape {
    Shape::Circle(_) => "round",
    Shape::Rect { .. } => "boxy",
}

// Push accumulator mutation into a small helper that returns the
// final value; the caller's binding stays `let`.
fn sum(xs: &[i64]) -> i64 {
    let mut acc = 0
    for n in xs { acc += n }
    acc
}
let total = sum(&xs)               // immutable at the call site
```

A `let mut` lives near a single update site (a loop, a builder, an
in-place sort) inside a small function that returns the new state.
If a binding is written from many places, break the function up.

### `if let` / `while let` - when to reach for them

```gossamer
if let Some(score) = scores.get(&name) {     // collapses a 4-line match
    println!("{name}: {score}")
}
while let Some(value) = rx.recv() { handle(value) }   // drain a channel
let mut cursor = err.cause()                          // walk a cause chain
while let Some(inner) = cursor {
    println!("  caused by: {}", inner.message())
    cursor = inner.cause()
}
if let Tree::Node(value, _, _) = node { println!("node = {value}") }
```

Use `match` (not `if let … else`) when you genuinely need every
variant.

### Let-chains - bind and test in one condition

An `if` / `while` condition may chain clauses with `&&`, where each
clause is either `let PAT = expr` or a boolean. Earlier `let`
bindings are in scope for later clauses and the body, so a nested
`match` collapses to one line. `||` cannot join `let` clauses without
parentheses (a `let` clause chain is `&&`-only).

```gossamer
if let Some(x) = a && let Some(y) = b && x > 0 {   // bind both, then test
    use(x + y)
}
if let Some(inner) = pair && let Some(v) = inner {  // later clause uses earlier bind
    println!("nested {v}")
}
while i < xs.len() && let n = xs[i] && n > 0 {       // while-let chain
    sum += n
    i += 1
}
```

## 3. The `|>` forward-pipe operator

Prefer `|>` whenever a value flows through two or more transforms.

- `x |> f` desugars to `f(x)`.
- `x |> f(a, b)` desugars to `f(a, b, x)` - piped value lands in the
  **last positional slot**.
- `x |> recv.m(a)` becomes `recv.m(a, x)` - pipe into an *external*
  receiver's last argument.
- `x |> _.m(a)` becomes `x.m(a)` - the **`_` placeholder** makes the
  piped value the *receiver*. `_` reads as "the value flowing through
  here". Bare `x |> _.trim` (no parens) is the nullary method call
  `x.trim()`; `x |> _.0` / `x |> _[i]` index the value; `x |> _` is the
  identity. Use this to pipe a value through its own methods:
  `s |> _.trim |> _.to_upper`.
- Left-associative, very low precedence: `a |> f |> g` reads as
  `g(f(a))` without parens.

```gossamer
fn double(x: i64) -> i64 { x * 2 }
fn add(a: i64, b: i64) -> i64 { a + b }
fn clamp(lo: i64, hi: i64, x: i64) -> i64 {
    if x < lo { lo } else if x > hi { hi } else { x }
}

let n = 3 |> double |> add(10) |> clamp(0, 100)   // reads top-down
```

A closure step threads the value into the last slot too:

```gossamer
let result = input |> parse_header |> validate |> |row| { row.body } |> write_out
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

fn sum(xs: &[i64]) -> i64 {
    let mut acc = 0
    for n in xs { acc += n }
    acc
}

fn main() {
    let total = sum(&[1, 2, 3])
    println!("total: {}", total)
}
```

## 5. Grammar essentials

- **Comments**: `//` line and `/* ... */` block are the only forms
  - block comments do **not** nest; there is no `///` / `//!`. A run
  of `//` lines immediately above an item (no blank line) is its
  documentation; `gos doc` renders these and `gos test` runs fenced
  code inside them.
- **Semicolons** are optional; one statement per line. A newline
  followed by a leading `&`, `*`, or `-` starts a new statement, so
  for multi-line continuation put the operator at the end of the
  previous line (`let x = a -\n  b`) or parenthesize.
- **Imports.** `use std::iter`; group with braces - `use std::{iter,
  os, strings}`. No trailing `;`. Alias with `use
  std::collections::{HashMap as Map}`. Paths validate against the
  canonical std manifest (`GR0005`): always spell the full path
  (`std::encoding::json`, not `std::json`).
- **Cross-module paths** in a multi-file package: `super::item` (parent
  module), `crate::path::item` (from the package root), and
  `self::child::item` (an explicit child) navigate between sibling/nested
  `mod` files declared per §13's project layout.
- **Expressions-as-statements.** `if`, `match`, `loop`, and blocks
  all yield values.
- **Entry file may omit `fn main`.** Bare statements at the top level of
  the entry file become the body of an implicit `fn main()`; items
  declared alongside are hoisted out. `?` makes the implicit main return
  `Result<(), errors::Error>`; set an exit code with `process::exit(n)`.
  Only the entry file (the file run directly, or `[project] entry`) may do
  this, and it cannot also declare an explicit `fn main`.
- **Bindings.** `let name = expr`, `let mut name = expr`, `let
  Point { x, y } = p`, `let (a, b) = pair`.
- **Functional record update.** `Type { ..base, field: value }` spreads
  a base value and overrides named fields; the `..base` may appear in
  any position (`{ ..base, x: 1 }` or `{ x: 1, ..base }`), explicit
  fields win, and only one spread is allowed. Base-copied fields keep
  the base usable afterward (its heap children are retained).
- **Generic bounds (static dispatch).** `fn f<T: Trait>(x: &T)` calls
  `Trait`'s methods on `x`; each call site instantiates `T`
  independently, an argument that does not `impl Trait` is a `GT0017`
  error, and every instantiation monomorphises to the concrete impl on
  all three tiers. Single-bound struct-typed parameters today; no
  `dyn Trait`, operator traits, associated types, or supertrait method
  inheritance through the bound.
- **Generic structs.** `struct Wrapper<T> { value: T }` with `impl<T>
  Wrapper<T> { fn get(&self) -> T { self.value } }` - fields and impl
  methods are generic over `T`, monomorphised per instantiation on all
  three tiers, same as generic functions.
- **Const-generic array length.** `fn sum<const N: usize>(xs: [i64; N])
  -> i64` takes a fixed-size array of any length; `N` is inferred from
  the argument's length and the function monomorphises correctly on all
  three tiers. The body iterates `xs` and reads `xs.len()`, `N` may
  appear in the return type (`-> [i64; N]`), and multiple const params
  (`<const N: usize, const M: usize>`) instantiate independently. Scope:
  `N` is inferred from a `[T; N]` argument; it is not yet usable as a
  bare value expression in the body or as a repeat count (`[0; N]`).
- **References.** `&x` read-shared, `&mut x` exclusive write -
  aliasing intent only; the runtime owns memory. **No lifetimes, no
  borrow checker.**
- **Types.** `bool`, `char`, `i8..i64`, `u8..u64`, `isize`,
  `usize`, `f32`, `f64`, `String`, `[T]`, `(A, B)`, `Option<T>`,
  `Result<T, E>`, `&T`, `&mut T`, user types. `i128` / `u128` are
  rejected (`GT0014`) - no tier has a 128-bit representation. Nested
  generics parse (`Vec<Vec<T>>`, `HashMap<String, Vec<i64>>`).
- **Transparent type aliases.** `type Id = i64` / `type Pair<A> = (A,
  A)` - the alias is interchangeable with its target everywhere (let
  bindings, params, returns, fields, composites, alias chains), and a
  generic alias substitutes its use-site arguments. A cyclic alias is
  rejected at check (`GT0024`).
- **`defer expr`** - runs on block exit by any path, LIFO, every tier.
- **Integer literals** are bare; inference picks the type, default
  `i64`. Suffix only with no contextual hint.
- **Byte literals.** `b'A'` is a `u8` (`65`), for byte-level work
  (parsing, hashing, binary protocols).
- **Casts.** `x as i32` - whitelist-checked (numeric ↔ numeric,
  `bool`/`char` → int, `u8` → `char`, same-type no-op). Int → narrow
  int masks at width (`300 as u8 == 44`); float → int truncates
  toward zero, saturates at i64 width, no narrow mask (`300.7 as u8
  == 300`, NaN → 0). Other `as` shapes are GT0005; `as i128/u128` is
  GT0014.
- **Patterns.** `_`, literals, `name`, `mut name`, `Variant(…)`,
  `Struct { … }`, tuples `(a, b)`, ranges - closed `1..=5`, exclusive
  `1..5`, and open-ended `..=hi` / `..hi` / `lo..` / `lo..=` (an open
  end covers up to the type maximum) - or-patterns `a | b`, `@`-bindings
  `x @ 1..=3`, rest `..`. Range patterns are opaque to exhaustiveness, so
  a `_` arm is still required; `..=` with no upper bound is a parse error.
  Guards: `Some(n) if n > 0 => …`. Used in `let`, `for`, params, `match`,
  `if let`, `while let`. Irrefutable `let` destructuring binds struct
  patterns (`let Point { x, y } = p`, renamed `let Point { x: a, y: b }
  = p`), nested structs, enum / tuple-struct variants (`let Shape::Pair(m,
  n) = s`), and or-patterns (`let (A(g, _) | B(g)) = v`, alternatives must
  bind the same names) on every tier. Rest patterns commonly slice a `Vec`
  head/tail: `if let [first, ..rest] = xs { use(first, rest) }`.
- **`if let` / `while let`** desugar to `match` - shorter reading,
  no new behavior.
- **`let PAT = expr else { … }`** - the else block must diverge
  (`return` / `break` / `continue` / `panic!`).

## 6. The built-in macros

Six format-shaped macros, plus a few fixed desugar / build-time
macros. Every other `name!(…)` is a parse error - no user-defined
macros.

| Macro | Returns | Destination |
|-------|---------|-------------|
| `format!("…", a, b)` | `String` | - |
| `println!("…", a, b)` | `()` | stdout + newline |
| `print!("…", a, b)` | `()` | stdout, no newline |
| `eprintln!("…", a, b)` | `()` | stderr + newline |
| `eprint!("…", a, b)` | `()` | stderr, no newline |
| `panic!("…", a, b)` | `!` | unwinds with the rendered message |

Plus the desugar macros: `matches!(e, pat)` (boolean pattern test),
`todo!` / `unimplemented!` / `unreachable!` (panic with a fixed or
given message), and `dbg!(e)` (prints `e` with `{:?}` to stderr,
yields its value). Build-time: `regex!` / `sql!` (validate the literal
at compile time) and `codegen!` (splice a `comptime fn`'s `String`).

Metaprogramming is Zig-style `comptime`, not macros: `comptime { … }`
blocks, `comptime fn` calls, and `comptime` params run on the bytecode
VM during compilation and fold to a literal, so every tier compiles the
identical constant. `typeInfo::<T>()` reflects a type's fields, and a plain
`for (name, ty) in typeInfo::<T>() { … }` loop is unrolled
per field at compile time into ordinary native code (`field_of(v,
name)` projects each field) - the basis for reflection-driven
serializers written once as `fn rec<T>(v: T) { … }` and specialized
per turbofish call site.

Rust-style `{}` placeholders plus named-capture `{ident}` for
bindings in scope:

```gossamer
let name = "jane"
println!("hello, {name}!")
println!("value: {} / {}", answer, total)
let greeting = "hello, " + &name      // `+` concatenates, no separator
```

Format specs follow Rust's `{:spec}` grammar - width and alignment
(`{:>8}` / `{:<8}` / `{:^8}` / `{:8}`), fill chars (`{:*>8}`),
zero-pad (`{:08}`), radix (`{:x}` / `{:X}` / `{:b}` / `{:o}`), and
precision (`{:.2}`, `{:>8.2}`), for positional and named (`{n:03}`)
arguments:

```gossamer
println!("[{:>8}]", 42)        // [      42]
println!("[{:08x}]", 255)      // [000000ff]
println!("[{:^6}]", "hi")      // [  hi  ]
println!("[{:>8.2}]", 3.14159) // [    3.14]
```

## 7. Error handling

Fallible functions return `Result<T, E>`. Propagate with `?`; build
/ wrap / inspect through `std::errors`:

```gossamer
use std::errors
use std::fs

fn load_config(path: &String) -> Result<String, errors::Error> {
    fs::read_to_string(path)
        .map_err(|e| errors::wrap(e, format!("reading {}", path)))
}
```

- `errors::new(msg)` - free-standing error.
- `errors::newf(fmt, args…)` - format-shaped constructor, e.g.
  `errors::newf("status {}", code)`.
- `errors::wrap(cause, msg)` - add a higher-level message.
- `errors::is(err, needle)` - test the cause chain for a needle;
  walk the chain directly with `err.cause()`.
- `errors::join([err, err])` - combine several.

Rendering a wrapped error with `{}` prints the colon-joined chain
(`outer: mid: root`); a joined error joins parts with `"; "`.
`.message()` is the top message only.

`?` also propagates `Option<T>` inside an `Option`-returning
function, and Result `?` auto-converts the error through `From`.

Idiomatic shape - fallible work returns `Result`, piped through
`result::map` for the ok-path and `result::default_with` to handle
the error in-line:

```gossamer
use std::{env, errors, fs, iter, result}

fn cat(f: &String) -> Result<(), errors::Error> {
    fs::read_to_string(f) |> result::map(|s| print!("{}", s))
}

fn main() {
    env::args() |> iter::for_each(|f| cat(&f) |> result::default_with(|e| eprintln!("{f}: {e}")))
}
```

`result::map(fn, r)` transforms `Ok(v)`, leaving `Err` untouched;
`result::default_with(fn, r)` handles the error and returns `()`.
Data-last argument order threads both through `|>`. `?` also works
inside macro arguments (`print!("{}", expr?)`).

Panics are goroutine-scoped: a panic in a spawned goroutine ends
only that goroutine - the scheduler keeps running - while a panic on
the main goroutine is fatal, as in Rust. Reserve for invariant
violations. Integer divide / modulo by zero panics (`GX0005`) on
every tier; `i64::MIN / -1` wraps to `i64::MIN`. Deep recursion in a
goroutine, closure, or method body raises a clean stack-overflow
(`GX0008`) rather than crashing - for genuinely deep recursion use
`gos build`, where native code lets the OS grow the stack.

## 8. Concurrency

`go expr` is fire-and-forget. When you need the result, `spawn(f)`
runs `f` on a goroutine and returns a `JoinHandle<T>`;
`handle.join()` blocks for `Result<T, String>` - `Ok(value)` or
`Err(message)` on panic. Closures capture their environment.

```gossamer
let h = spawn(|| compute())
match h.join() {
    Ok(v) => println!("{}", v),
    Err(e) => eprintln!("worker failed: {}", e),
}
```

Typed channels via `std::sync::channel()`. `recv()` blocks until a
value arrives or every sender is gone; `close()` ends the stream, so
`while let Some(v) = rx.recv()` is the canonical drain - no sleeps,
identical under `gos run` and `gos build`:

```gossamer
use std::sync::channel

fn produce(tx: Sender<i64>) {
    tx.send(1); tx.send(2); tx.send(3)
    tx.close()
}

fn main() {
    let (tx, rx) = channel()
    go produce(tx)
    let mut total = 0
    while let Some(v) = rx.recv() { total += v }
    println!("total: {}", total)
}
```

`select { }` multiplexes receives/sends; arms poll in source order,
the goroutine parks until one is ready (or a `default` arm fires):

```gossamer
select {
    x = rx_a.recv() => handle_a(x),
    y = rx_b.recv() => handle_b(y),
    default => do_something_else(),
}
```

Prefer channels for coordination; `sync::Mutex` only when
shared-memory updates are simpler. Close the channel from the
producer when the stream ends.

## 8a. Closures and higher-order fns

Lambdas: `|param: T| body`. Captures work as expected
(runtime-managed, no `move`). Two callable types:

- `fn(args) -> ret` - raw code pointer; accepts only non-capturing
  items.
- `Fn(args) -> ret` - callable trait; accepts bare items and
  capturing closures (fat pointer; implicit conversion at the call
  site).

```gossamer
fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }

fn main() {
    let scale = 10
    let scaled = |y: i64| scale * y     // captures `scale`
    println!("{}", apply(scaled, 5))    // 50
    fn add_one(y: i64) -> i64 { y + 1 }
    println!("{}", apply(add_one, 41))  // 42 - bare fn coerces
}
```

Single trait variant - no `FnMut` / `FnOnce` distinction (they parse
but lower to the same `Fn(_)` shape).

## 8b. Iterators

Any type with `fn next(&mut self) -> Option<T>` is iterable; `for x
in ...` desugars to `{ let mut __iter = expr; loop { match (&mut
__iter).next() { Some(x) => body, None => break } } }`.

```gossamer
struct Counter { next_value: i64, end: i64 }
trait Iterator { fn next(&mut self) -> Option<i64> }
impl Iterator for Counter {
    fn next(&mut self) -> Option<i64> {
        if self.next_value < self.end {
            let v = self.next_value
            self.next_value = self.next_value + 1
            Some(v)
        } else { None }
    }
}
```

`std::iter` also exposes a lazy `Lazy` adapter over any Rust
`Iterator`: `map` / `filter` / `take` / `skip` / `step_by` adapters,
`to_vec` / `sum` / `min` / `max` / `count` / `any` / `all`
terminals - allocation-free until the terminal.

## 9. Data structures

- `[T]` - growable array. `[1, 2, 3]`; `for x in xs`; mutate with
  `push`, `pop`, `swap(i, j)`, `sort()`, `sort_by(|a, b| …)`.
- `[T; N]` - fixed-size. `[v; N]` (repeat) or `[a, b, c]`.
  Stack-allocatable; pick when the size is a compile-time constant.
- `(A, B, …)` - tuple. `.0`, `.1`, … or destructure inline.
- `struct Foo { x, y }` / `struct Pair(A, B)` - runtime-managed
  value types. Tuple structs are fully usable: construct `Pair(1, 2)`,
  read `p.0`, destructure `let Pair(a, b) = p`, derive, and serde.
- `enum E { A, B(Payload) }` - sum types, matched exhaustively.
  Recursive payloads work directly; `Box`/`Arc`/`Rc` transparent.
- `Option<T>` - `Some` / `None`, read with `if let`. `Result<T, E>`
  - `Ok` / `Err`, propagate with `?`.
- `std::collections::{Vec, HashMap, HashSet, BTreeMap, VecDeque}` - the
  richer containers. `HashMap`: `m.inc(k)` / `m.inc(k, by)`,
  `m.or_insert(k, default)`, `m.iter()` (yields `[(K, V)]`), `keys()` /
  `values()`, `HashMap::pop(m, k) -> Option<V>`. Structs and tuples work
  as keys, keyed by value on every tier. `BTreeMap` keeps keys sorted
  and takes `String` or `i64` keys. `VecDeque` is a double-ended queue:
  `push_back` / `push_front` / `pop_back` / `pop_front` / `peek_front` /
  `peek_back` / `len`.
- `Vec` methods: `contains(&v)`, `index_of(&v) -> Option<i64>`,
  `count_of(&v)`, `first()` / `last() -> Option<T>`, `reversed()`
  (non-mutating; `reverse()` is in-place), `xs.slice(start, end) ->
  Result<Vec<T>, errors::Error>` (out-of-range → Err). The
  Result-returning `Vec::insert(xs, i, v)` / `Vec::remove(xs, i)` are
  qualified free functions; method-call `xs.insert/remove` keep
  silent in-place semantics.
- Collection literals coerce to `Vec<T>` / `[T]` wherever the
  expected type calls for one (let annotation, return, field, arg);
  `if`/`match` branches of differing lengths join to `Vec<T>`.
- Enums cap at 256 variants (`GT0012`).

## 9a. Weak references (breaking cycles)

Reference counting means a genuine cycle (parent <-> child, doubly-linked
list) leaks unless one direction holds a non-owning pointer. `Weak<T>`
is that non-owning pointer: `let w = strong.downgrade()` produces a
`Weak<T>` from any `Arc`/`Rc`-backed value; `w.upgrade() -> Option<T>`
gets a strong handle back, or `None` once every strong owner is gone.
`std::runtime::collect_cycles()` runs the cycle collector on demand
(the LLVM/native tier also runs it automatically; see the tier-divergence
note in §14).

```gossamer
struct Node { name: String, parent: Weak<Node>, children: [Node] }
```

**Tier-divergence trap:** a `Weak` observing a member of a genuine
*strong* cycle reads as still-live (`Some`) under `gos run` (the
interpreter's cycle collector is a no-op there), but as `None` under
`gos build` / `gos build --release` once the Bacon-Rajan collector
actually runs. Don't rely on a `Weak` into a strong cycle resolving
consistently across tiers - break the cycle for real, and cross-check
with `gos build` whenever `Weak` behavior matters.

## 10. The `gos` toolchain

Every subcommand takes a `.gos` file or a project dir. Bare `gos`
opens the REPL. In a project, `gos run` / `gos build` with no path
resolve the entry themselves (`src/main.gos`, `main.gos`, the
manifest-id-named source, then a sole `.gos` candidate).

| Command | Purpose |
|---------|---------|
| `gos check FILE` | Parse + resolve + typecheck + exhaustiveness. |
| `gos parse FILE` | Dump the AST. |
| `gos run FILE` | Register-based bytecode VM (with in-process JIT). |
| `gos build FILE` | Native build via LLVM (`llc -O0`) + system linker. Fast compile, unoptimised. |
| `gos build --release FILE` | Full LLVM pipeline (`opt -O3 \| llc -O3`), static-musl on Linux. Strict lowering by default (`--allow-llvm-fallback` opts out). `--target TRIPLE` cross-compiles to `{x86_64,aarch64}-unknown-linux-{gnu,musl}` (QEMU-validated, including Raspberry Pi) from any of Linux/macOS/Windows hosts - macOS/Windows-as-*target* isn't supported yet; `-g` embeds DWARF. |
| `gos test PATH` | Run `#[test]` functions. `--coverage <path>` (lcov), `--parallel N` / `--serial`, `--format junit`, `--tier-parity`. |
| `gos bench PATH` | Time `#[bench]` functions. |
| `gos fmt [--check] FILE` | Token-stream formatter; idempotent, comment/macro/line-structure preserving. |
| `gos doc FILE` | Item listing + doc comments. |
| `gos lint [--deny-warnings] PATH` | Lint suite. |
| `gos explain CODE` | Long-form rationale for a diagnostic code. |
| `gos watch --command CMD PATH` | Re-run on file change. |
| `gos clean [--vendor] [--dry-run]` | Remove `target/`, caches; `--vendor` drops `vendor/`. |
| `gos new ID --path DIR [--template T]` | Scaffold a project. `--template` is `bin` (default), `lib`, `service` (binds `0.0.0.0:8080`), `workspace` (`[workspace]`/`members`, no source tree), or `binding` (Rust-binding crate skeleton). |
| `gos init ID` | Scaffold just `project.toml` in the current directory - lighter than `gos new`. |
| `gos bindgen INPUT --out DIR` | Scaffold a `#[gos_module]` Rust-binding crate skeleton from a Rust source file. |
| `gos add SPEC` / `remove` / `tidy` / `fetch` / `vendor` | Package manager. |
| `gos publish` / `yank` / `login` / `logout` / `owner` | Registry workflow (Ed25519-signed tarballs, sha256 pinned in the lockfile). |
| `gos feature-status [--check]` | List the feature-status registry (see §14 for current non-Shipped entries). |
| `gos repl` | Explicit name for bare-`gos`'s REPL. |
| `gos lsp` | stdio LSP adapter for editors. |
| `gos env` | Print toolchain diagnostics: version, runtime lib path, host triple, `cc` path. |
| `gos completion {bash,zsh,fish,...}` | Generate shell completions. |
| `gos skill-prompt` | Print this skill card verbatim (it's compiled into the CLI via `include_str!`) - e.g. `gos skill-prompt \| claude --append-system-prompt`. Keeping this file accurate has a direct, mechanical payoff. |

## 11. Writing tests

Unit tests live in the file they cover under `#[cfg(test)] mod
<file>_tests { … }`, reaching the file's own items via `super::`.
**In a multi-file project give each file's test module a unique name**
(`util_tests`, `parser_tests`, …): several `mod tests` across bundled
siblings collide on `gos build`/`gos run` (`GR0003`). `gos test` bundles
sibling modules the same way `gos run` / `gos build` do, so a `#[test]`
may call a sibling module declared `mod NAME;` via `super::NAME::item`.
`assert(cond[, msg])` / `assert_eq(a, b[, msg])` are prelude builtins
(panic on failure; a pass is counted in the tally); `std::testing::check*`
record without panicking.

```gossamer
pub fn add(a: i64, b: i64) -> i64 { a + b }

#[cfg(test)]
mod arith_tests {
    use std::testing
    #[test]
    fn add_adds() {
        testing::check_eq(&super::add(2, 3), &5, "2+3")
    }
}
```

Doc-tests: fenced code inside a `//` doc-comment block is compiled
and run by `gos test`. Mark non-runnable fences ` ```text ```.

## 12. Standard library surface

Tight index of the common surface. Many modules are large; trust
repo examples and write a small test when unsure.

- `std::fmt` - `Display`, `Debug`.
- `std::io` - `Read`, `Write`, buffered wrappers, `stdin` / `stdout`.
  Blocking line input: `os::stdin().read_line() -> Option<String>`
  (trailing newline stripped, `None` on EOF).
- `std::env` - `args`, `program_name`, `var`, `set_var`,
  `unset_var`, `current_dir`, `set_current_dir`, `home_dir`,
  `temp_dir`.
- `std::process` - the callable surface is free functions, **not** a
  `Command` builder (`Command`/`Output`/`Stdio`/`Child`/`ExitStatus` are
  Rust-only types for native extension authors, unbound in `.gos`
  - `process::Command::new(...)` is a `GX0002` error). Use
  `process::run(prog, args) -> Result<{stdout: String, stderr: String,
  code: i64}, String>`: no shell involved (real exec, so argv elements
  need no escaping), `prog`/`args` take bare values or `&`-refs. Also
  `spawn`, `kill`, `exit`, `id`, `abort`, `pipeline_run`, `signal`,
  `kill_group`, `wait_timeout` (POSIX-only) - no stdin piping and no
  builder for those either.
- `std::fs` - `read`, `read_to_string`, `write`, `read_dir`,
  `walk_dir`, `create_dir(_all)`, `remove_file/dir(_all)`,
  `remove_all`, `copy`, `rename`, `exists`, `is_file/dir/symlink`,
  `file_size`, `metadata`, `canonicalize`.
- `std::path` - pure manipulation (no I/O): `join`, `split`, `base`,
  `dir`, `ext`, `clean`, `is_absolute`, `has_prefix`, `matches`,
  `parent`, `file_name`, `stem`, `normalize`.
- `std::os` - `family()`, `arch()`; `write_file(path, &Vec<u8>)`
  (binary-safe) and `read_file(path) -> Result<Vec<u8>, _>` /
  `read_file_to_string`. `std::os::user` - POSIX user/group lookup.
  `std::os::signal` - OS signal handling.
- `std::strings` - `split`, `splitn`, `split_whitespace`, `trim(_start/_end)`,
  char-set trims `trim_matches(set)` (both ends) / `trim_start_matches(set)` /
  `trim_end_matches(set)` (there is **no** `strip_chars`/`lstrip_chars`/
  `rstrip_chars` - those raise `GX0002`), `contains`, `find`, `rfind`,
  `replace`, `replacen`,
  `to_lower/upper`, `to_title`, `starts_with`, `ends_with`, `repeat`,
  `lines`, `join`, `strip_prefix/suffix`, `pad_left/right`. Also as
  `String` methods: `split_once(sep) -> Option<(String, String)>`,
  `rsplit_once`, `count(needle)`, `find_any(chars)`/`rfind_any(chars)
  -> Option<i64>`, `center(w, c)`, `slice(a, b) -> Result<String, _>`,
  `substring(a, b) -> String` (out-of-range clamps), `byte_at(i) -> i64`
  (0 outside `[0, len)`). Prefer `to_lower` over `to_lowercase`. Both
  `strings::join(&parts, sep)` and `parts.join(sep)` on `[String]` work.
- `std::strconv` - `parse_int/i64/u64/float/f64/bool`,
  `format_int/i64/float/f64`, `itoa`, `atoi`, `parse_i64_radix(s, base)`
  / `format_i64_radix(n, base)` (bases 2..=36), `quote`/`unquote`.
- `std::utf8` - `count_runes`, `rune_count(_in_string)`, `rune_len`,
  `is_valid`, `valid_rune/string`, `full_rune(_in_string)`,
  `rune_start`, `decode_rune/last_rune/first` (+ `_in_string`),
  `encode_rune`, `append_rune`.
- `std::unicode` - full Unicode 16: category predicates
  (`is_letter/digit/number/space/upper/lower/title/punct/symbol/...`,
  `combining_class`); casing (`to_upper/lower/title`, `simple_fold`,
  `fold_case`, `to_upper_str/lower_str`); normalization
  (`nfc/nfd/nfkc/nfkd`); segmentation (`graphemes`, `words`,
  `sentences` + count variants). UAX #31 identifiers: `let café = 1`.
- `std::collections` - `Vec`, `HashMap`, `HashSet` (real set:
  `insert`, `remove`, `contains`, `len`, `is_empty`, `clear`,
  `to_vec`, `iter`; algebra: `union`, `intersection`, `difference`,
  `symmetric_difference`, `is_subset`, `is_superset`, `is_disjoint`),
  `BTreeMap` (sorted, `String` or `i64` keys), `VecDeque` (double-ended:
  `push_back/front`, `pop_back/front`, `peek_front/back`). (Vec/HashMap
  method extras under §9.) A separate, `i64`-only family -
  `queue`/`stack`/`deque`/`heap`/`ordered_set`/`ordered_map`/
  `ordered_vec` - is **functional/re-bind style**, not mutating-in-place:
  `let q = queue::push(q, v)` returns the updated collection rather than
  mutating `q`. Reach for these when that immutable-update shape fits;
  otherwise prefer the mutating `Vec`/`HashMap`/`VecDeque` above.
- `std::net` - `TcpListener::{bind, accept, local_addr, close}`,
  `TcpStream::{connect, read, read_to_string, write, close}`,
  `UdpSocket::{bind, send_to, recv_from, local_addr, close}`,
  `resolve` / `lookup` (DNS). `UnixListener` / `UnixStream` - Unix-domain
  sockets (`#[cfg(unix)]`, POSIX-only). `std::net::url` - parse + render +
  escape. `std::net::netip` - typed IP address/port (Go `net/netip`
  shape). `std::net::ip` - string-level IPv4/IPv6 parse/classify.
  `std::mime` - RFC 2045 media-type parsing and extension lookup.
- `std::http` - `Method`, `StatusCode`, `Headers`, `Request`,
  `Response`, `Handler`, `serve` (returns `Result<(), Error>`). HTTP
  client: `Client::builder().max_redirects(n).timeout_ms(ms).build()`;
  free wrappers `http::get/post/put/delete/options/head/request/
  request_bytes`; `stream(...)` → `ResponseStream` (`next_line()`,
  `next_chunk(max)`) for SSE/chunked. `Response`: `status`, `body`,
  `raw_bytes`, `content_type`, `location`, `headers: [(String, String)]`.
  Server: `Request.raw_body ([u8])`, `r.path` strips query (`r.query`
  keeps it); `Response::text/json`, `Response::with_header(k, v)`,
  `Response::stream(status, ct, upstream)`; bodies cap at 1 MiB.
- `std::http` server stack: `cookie`, `csrf`, `form`, `multipart`,
  `query`, `session`, `state` (`AppState`/`State<T>`), `health`,
  `middleware` (`accepts_gzip`, `bearer_ok`, `decode_basic_auth`,
  `new_request_id`, `tag`); HTTP/2 push + trailers. `std::http_h3` -
  HTTP/3 server + client (RFC 9114). `std::http::websocket` - first-party
  RFC 6455 WebSocket (`serve`/`connect`/`send_text`/`send_binary`/`recv`/
  `close`; no `wss://` yet). `std::http::static_files` - caching
  static-file handler (ETag/Last-Modified/Range/MIME sniff).
  `std::http::proxy` - reverse proxy built on `http::Client`.
  `std::html` - HTML escape/unescape. `std::html::template` and
  `std::text::template` (both Experimental, see §14) - context-aware
  auto-escaping HTML templates and plain-text templates. `std::tls`
  (Experimental) - rustls-backed TLS client/server (`start_tls`,
  `start_tls_ca`, `start_tls_insecure`, `http::serve_tls`).
- `std::http::router` - `Router::new()` returns a `Router`; all verb
  methods (`get`, `post`, `put`, `delete`, `patch`, `head`, `options`
  and their `_fn` closure variants) return the same `Router`, so they
  chain naturally with `|>`. Path params are read via
  `r.path_value("name")` / `r.path_int("id")`.

  ```gossamer
  use std::http
  use std::http::router

  let r = router::Router::new()
      |> _.get("/", handler_fn)
      |> _.post("/items", create_fn)
      |> _.get_fn("/ping", |_r| Ok(http::Response::text(200, "ok")))
  http::serve("0.0.0.0:8080", r)?
  ```
- `std::encoding::{json, base64, hex, binary}`. Every user struct
  gets generic serializer free functions called with a turbofish:
  `from_json::<Type>(text) -> Result<Type, _>` and
  `to_json::<Type>(value) -> Result<String, _>` (the single
  spelling - no `Type::from_json` methods). The decoder validates
  each field against its declared type with path-qualified errors;
  nested structs and `[T]`/`Vec<T>` of a supported inner type walk
  recursively. **`Option<T>`, `HashMap<...>`, tuples, `[T; N]`, and a
  `json::Value` field are not supported in the struct derive today** -
  a struct with any such field makes `from_json`/`to_json` fail to
  exist for it at all (`GR0001: cannot find` the synthesized
  serializer), not a narrower per-field error. `let user: User =
  from_json::<User>(&text)?` is canonical for fully-scalar/nested-struct
  shapes; unknown extra JSON keys are ignored, not rejected. For any
  document with a dynamic or partially-known shape (which includes the
  above unsupported field types), decode the whole thing with the
  dynamic API instead: `json::parse(&text) -> Result<Value, Error>`,
  `json::get(&value, key) -> Option<&Value>`, `json::at(&value, idx) ->
  Option<&Value>`, `json::as_i64/f64/str/bool(&value) -> Option<T>`,
  `json::is_null(&value) -> bool`, `json::len(&value) -> i64`,
  `json::keys(&value) -> Vec<String>`. Narrow int fields in the typed
  path round-trip via `as`.
  Also `std::encoding::{xml, base32, ascii85, csv, pem}` for those formats.
- `std::encoding::yaml` - YAML 1.2 parse/encode + `yaml::to_json` /
  `from_json` text converters; auto-derived `from_yaml::<T>` /
  `to_yaml::<T>` on every struct compose these with the JSON pair.
  Also `encoding::toml` (`toml::to_json` / `from_json`).
- `std::database::sql` - driver-pluggable SQL (no driver bundled;
  implement `Driver` via `[rust-bindings]`). `open(driver, url) ->
  Result<Conn, _>`; `Conn`: `execute/query/query_each/prepare`,
  `begin/begin_with(IsolationLevel)`, `copy_in/copy_out`,
  `listen/unlisten/poll_notification`, `ping`, `close`; `Tx`:
  `commit/rollback/savepoints`; `Rows.next_row() -> Option<Row>`
  (cursor; `defer rows.close()`), `columns()`; `Row`:
  `get_i64/f64/bool/text/blob`, `get_opt_*`, `is_null`, `width`;
  `Value` (Null/Bool/Int/Float/Text/Blob), positional `$N`; `Pool`;
  `migrate::up(&mut conn, dir) -> i64`; `Select` fluent builder.
- `std::sync` - `Mutex`, `RwLock`, atomics, `channel`, `Once`,
  `WaitGroup` (`new`/`add`/`done`/`wait`), `Barrier` (thread-rendezvous),
  `Map` (concurrent string→string: `set`/`get`/`delete`/`len`/`contains`/
  `keys`). For non-string payloads wrap a `HashMap` in `Mutex`.
  `std::thread` - native OS threads, distinct from `go`/goroutines.
- `std::time` - `Instant::{now, elapsed_ms}`, `Duration::{from_millis/secs/micros,
  as_millis/secs/micros}`, `sleep`, `now`, `now_ms` (unix milliseconds,
  the stable integer to use for e.g. a timestamped directory name),
  `now_nanos`, `monotonic_ms/nanos`, `since_ms`, `format_rfc3339`,
  `parse_rfc3339`.
  Channel timer: `after(d) -> Receiver` (one-shot) - drain with
  `while let` or use as a `select` timeout arm.
- `std::context` - cancellation, deadlines, `Context::background()`.
- `std::bytes` / `std::bufio` - binary buffers and buffered IO.
- `std::flag` - CLI flag parser. `flag::Set::new(prog_name)` then
  `fs.string/int/bool(name, default, summary) -> flag::Cell<T>` per
  flag; `flag::Cell<T>` auto-derefs at every value-context (comparisons,
  call args, `if`, `println!`), explicit `*cell` still works. `fs.parse(&
  args) -> Result<Vec<String>, flag::Error>` returns the positional
  args (skips `args[0]`, honors `--`) and handles `--help`/`-h` itself
  (prints auto-generated usage, returns `Ok([])`). No built-in
  "required" flag or mutual-exclusion primitive - check that yourself
  after `parse()`.
- **Scalar `min` / `max` / `clamp`** - bare prelude functions, no
  import. `min(3, 7) == 3`, `clamp(15, 0, 10) == 10`. Vec-shaped
  `min(xs)` / `max(xs)` return `Option<T>`.
- `std::sort`, `std::math::rand` (deterministic RNG), `std::math::big`
  (arbitrary-precision integers).
- `std::crypto::{rand, sha256, sha512, hmac, subtle, blake3, aead,
  ed25519, ecdsa, x509, kdf, cipher}` - narrow, audited (`cipher` covers
  AES/CBC/CTR); `crypto::password` - Argon2id (`hash`, `verify`,
  `needs_rehash`, PHC strings). `crypto::insecure` - MD5/SHA1,
  compat-only, not for new code.
- `std::hash::{fnv, crc32, adler32}` - non-cryptographic hashes and
  checksums.
- `std::uuid` - UUID v4/v7 generate, parse, normalize.
- `std::jwt` - RFC 7519 sign/verify HS256/384/512, ES256, EdDSA,
  RS256/384/512 (verify): `sign_hs`/`verify_hs`,
  `sign_es256`/`verify_es256`, `sign_eddsa`/`verify_eddsa`.
- `std::metrics` - Prometheus `Counter`/`Gauge`/`Histogram` +
  `Registry`. `std::trace` - W3C trace-context + OTLP JSON exporter.
- `std::compress::{gzip, flate, zlib, zstd, bzip2}` - byte-in/byte-out
  (zstd 1-22, default 3).
- `std::archive::{zip, tar}` - archive read/write.
- `std::lifecycle` - graceful shutdown, signals, sd_notify.
  `std::validate` - `Validate` trait + `FieldError` / `Errors`.
  `std::slog` - structured logging.
- `std::runtime` - `collect_cycles()`, `arena_push/pop` (prefer the
  `arena {}` block), `set_panic_hook(f: fn(String))`. Main-goroutine
  panic exits 101; an unobserved `go` panic prints one `error[GX0005]`
  line and ends only that goroutine.
- `std::testing` - `check`, `check_eq`, `Runner`, `check_ok`.
- `std::regex` - wraps the Rust `regex` crate; named groups via
  `capture_names(pat)`, `captures_named(pat, hay)` /
  `captures_named_all` → `HashMap<String, String>`.

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

```toml
[project]
id      = "example.com/widget"
version = "0.1.0"
authors = ["Jane Roe <jane@example.com>"]
license = "Apache-2.0"
# entry  = "src/app.gos"   # optional: override convention-based entry resolution

[dependencies]
"example.org/lib" = "1.2.3"

# [rust-bindings]           # optional: native Rust crates exposing #[gos_module]
# "example.org/pg-driver" = "0.4.0"
```

The optional `[project] entry` key names the entry source directly,
overriding the convention search; the resolved entry is the only file
allowed to carry top-level statements. `[rust-bindings]` declares native
Rust crates (scaffolded via `gos bindgen`) that back `std::database::sql`
drivers and similar FFI - distinct from `[dependencies]`, which are
Gossamer packages.

A multi-package checkout uses a workspace manifest instead of `[project]`
at the root (scaffolded via `gos new ID --path DIR --template workspace`):

```toml
[workspace]
members = ["packages/*"]
```

Qualified type-path annotations (`util::Rec` in params, `let` bindings,
and return types) resolve correctly to the struct's fields across sibling
modules on all tiers.

## 14. Current gaps (pre-1.0.0)

- `+` on `String` copies; for heavy assembly use
  `std::bytes::Builder` or a `mut String` with `+=`.
- Method dispatch is name-global in places. Qualified path calls
  (`Point::origin()`) always work, and `String` / `HashMap` / `Vec`
  receivers dispatch by type (a `String::` / `HashMap::` / `Vec::`
  key resolved ahead of the bare name), so `s.to_title()` reaches the
  string op, not `unicode::to_title`, and `parts.join(sep)` on a
  `[String]` reaches `strings::join`. Strings are values - bind or
  borrow instead of cloning, and don't shadow built-in method names.
- `u64` / `usize` at or above 2^63 compare, shift, and display as
  unsigned by their declared type on the bytecode VM and the LLVM AOT
  tier; `+` / `-` / `*` run at i64 width (identical bit results).
  Residual gap: the in-process Cranelift JIT still compares and shifts
  such values as signed, so a hot large-`u64` loop can differ between
  `gos run` and `gos build` - cross-check with `gos build` when it matters.
- **`gos feature-status` non-Shipped items today:** `std::database::sql`,
  `std::html::template`, `std::text::template`, `std::tls` are
  Experimental (usable, surface may still shift); `async`/`await`,
  explicit lifetimes, and the `move` closure keyword are Planned (not
  implemented - closures already capture by runtime-managed reference
  with no `move` needed, and there are no lifetimes to write).

### Tier-divergence traps

The surface runs bit-identically across the bytecode VM (`gos run` /
`gos test`), the Cranelift JIT, and the LLVM AOT tier (`gos build`).
When you hit something that behaves differently across tiers it is a
bug - reduce it and check against `gos test` (interpreter) **and**
`gos build` (LLVM). Two things remain genuinely tier-sensitive:

- **Per-file test modules must have unique names.** Multiple
  `#[cfg(test)] mod tests` across bundled sibling files collide on
  `gos build`/`gos run` with `GR0003: name 'tests' defined multiple
  times` - name them `mod foo_tests`, `mod bar_tests`, etc.
- **`Weak<T>` into a genuine strong cycle** (§9a) resolves `Some` under
  `gos run` (no-op cycle collector) but `None` under `gos build` once
  the real collector runs - this is a behavioral divergence, not just a
  source-level footgun; don't depend on which side you see without
  cross-checking `gos build`.
- **Entry-point `Err` is dropped, and diverges by tier** (open bug,
  verified against 0.23.0): when the entry point - either the implicit
  top-level-statement main or an explicit `fn main() -> Result<T, E>` -
  returns `Err(e)` via `?`, `e` is never printed on either tier, and the
  exit code diverges: `gos run` exits `0` (the `Result` is discarded
  outright), `gos build` exits `1`. Until this is fixed, don't rely on
  a bare `?`-propagating main for user-facing error reporting or exit
  codes - explicitly `match` at the entry point and call
  `process::exit(n)` with a printed message in the `Err` arm.
- **`fs::read_to_string`'s error path is dropped on the native/LLVM
  tier only** (open bug, verified against 0.23.0): a missing/unreadable
  path correctly returns `Err(...)` under `gos run`, but returns
  `Ok("")` under `gos build`. `fs::exists`, `fs::read` (bytes), and
  `fs::write`'s error paths are unaffected on both tiers. Guard with
  `fs::exists(&path)` before `fs::read_to_string` if the binary might
  ever be `gos build`-compiled.

## 15. Where to read more

- Language spec: `SPEC.md`. Style guide: `GUIDELINES.md`.
- Rendered docs: `docs_src/` → `docs/` (via `mkdocs build`).
- Examples: `examples/` - start with `hello_world.gos`,
  `function_piping.gos`, `go_spawn.gos`, `concurrency.gos`.

## 16. When in doubt

Run it. `gos check` gives rustc-class diagnostics with source
excerpts and did-you-mean suggestions; `gos explain <CODE>` expands
any diagnostic code. The toolchain is your first debugger.
