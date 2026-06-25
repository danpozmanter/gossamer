# Gossamer - Skill Card

Drop this file into a model's context to teach it how to write
idiomatic Gossamer. Self-contained. Covers: what Gossamer is,
surface syntax, forward-pipe style, the `gos` toolchain, error
handling, concurrency, stdlib surface, and how to test. No prior
context assumed.

---

## 1. What Gossamer is

A goroutine-powered, fast-compiling systems language with
automatic memory management (deterministic reference counting
plus `arena { }` regions - no borrow checker, no lifetimes, no
tracing-GC pauses). Syntax is Rust-flavoured. Runtime is
Go-shaped: goroutines, channels. Source files end in `.gos`. The
toolchain binary is `gos`. Every project ships a `project.toml`
manifest.

Status: pre-1.0.0 (currently 0.18.2). The surface is stable to
write against, and features ship across all three tiers (bytecode
VM, in-process JIT, LLVM AOT) - see "current gaps" at the bottom.

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
- **`#[derive(Clone, PartialEq, Eq, Default, Debug, Hash)]`** on
  structs and enums - synthesized as real source, so `==`,
  `.clone()`, `Type::default()`, `{:?}` work on every tier. Enums
  derive for tuple, unit, and struct-payload (`Rect { w, h }`)
  variants; `#[default]` picks the `Default` variant. Don't hand-roll
  field-wise eq/clone.
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
- **Bare numeric literals - always.** `0`, `200`, `1.5`, not
  `0i64` / `1.5f64`. Inference picks the type from binding, call
  site, or return type. Suffix only when a literal stands alone
  with no contextual hint. Same for indices: `arr[0]`.
- **String literals are already `String`.** Don't write
  `"foo".to_string()`. `&"foo"` borrows where `&String` / `&str`
  is expected.
- **Macros only for formatted output.** `println!`, `format!`,
  `print!`, `eprintln!`, `eprint!`, `panic!` - no others exist.

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
- **`defer expr`** - runs on block exit by any path, LIFO, every tier.
- **Integer literals** are bare; inference picks the type, default
  `i64`. Suffix only with no contextual hint.
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
  bind the same names) on every tier.
- **`if let` / `while let`** desugar to `match` - shorter reading,
  no new behavior.
- **`let PAT = expr else { … }`** - the else block must diverge
  (`return` / `break` / `continue` / `panic!`).

## 6. Formatted output (the only macros)

Exactly six macros, all format-shaped. Every other `name!(…)` is a
parse error.

| Macro | Returns | Destination |
|-------|---------|-------------|
| `format!("…", a, b)` | `String` | - |
| `println!("…", a, b)` | `()` | stdout + newline |
| `print!("…", a, b)` | `()` | stdout, no newline |
| `eprintln!("…", a, b)` | `()` | stderr + newline |
| `eprint!("…", a, b)` | `()` | stderr, no newline |
| `panic!("…", a, b)` | `!` | unwinds with the rendered message |

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
  value types.
- `enum E { A, B(Payload) }` - sum types, matched exhaustively.
  Recursive payloads work directly; `Box`/`Arc`/`Rc` transparent.
- `Option<T>` - `Some` / `None`, read with `if let`. `Result<T, E>`
  - `Ok` / `Err`, propagate with `?`.
- `std::collections::{Vec, HashMap, HashSet, BTreeMap}` - the richer
  containers. `HashMap`: `m.inc(k)` / `m.inc(k, by)`, `m.or_insert(k,
  default)`, `m.iter()` (yields `[(K, V)]`), `keys()` / `values()`,
  `HashMap::pop(m, k) -> Option<V>`. Structs and tuples work as
  keys, keyed by value on every tier.
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

## 10. The `gos` toolchain

Every subcommand takes a `.gos` file or a project dir. Bare `gos`
opens the REPL. In a project, `gos run` / `gos build` with no path
resolve the entry themselves (`src/main.gos`, `main.gos`, the
manifest-id-named source, then a sole `.gos` candidate).

| Command | Purpose |
|---------|---------|
| `gos check FILE` | Parse + resolve + typecheck + exhaustiveness. |
| `gos run FILE` | Register-based bytecode VM (with in-process JIT). |
| `gos build FILE` | Native build via LLVM (`llc -O0`) + system linker. Fast compile, unoptimised. |
| `gos build --release FILE` | Full LLVM pipeline (`opt -O3 \| llc -O3`), static-musl on Linux. Strict lowering by default (`--allow-llvm-fallback` opts out). `--target TRIPLE` cross-compiles; `-g` embeds DWARF. |
| `gos test PATH` | Run `#[test]` functions. `--coverage <path>` (lcov), `--parallel N` / `--serial`, `--format junit`, `--tier-parity`. |
| `gos bench PATH` | Time `#[bench]` functions. |
| `gos fmt [--check] FILE` | Token-stream formatter; idempotent, comment/macro/line-structure preserving. |
| `gos doc FILE` | Item listing + doc comments. |
| `gos lint [--deny-warnings] PATH` | Lint suite. |
| `gos explain CODE` | Long-form rationale for a diagnostic code. |
| `gos watch --command CMD PATH` | Re-run on file change. |
| `gos clean [--vendor] [--dry-run]` | Remove `target/`, caches; `--vendor` drops `vendor/`. |
| `gos new ID --path DIR` | Scaffold a project. |
| `gos add SPEC` / `remove` / `tidy` / `fetch` / `vendor` | Package manager. |
| `gos publish` / `yank` / `login` / `logout` / `owner` | Registry workflow (Ed25519-signed tarballs, sha256 pinned in the lockfile). |
| `gos feature-status` | List/`--check` the feature-status registry. |

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
- `std::env` - `args`, `program_name`, `var`, `set_var`,
  `unset_var`, `current_dir`, `set_current_dir`, `home_dir`,
  `temp_dir`.
- `std::process` - `Command`, `Output`, `Stdio`, `Child`,
  `ExitStatus`, `run`, `spawn`, `kill`, `exit`, `id`, `abort`,
  `Pipeline` (`pipeline_run`), `Signal`, `signal`, `kill_group`,
  `wait_timeout` (POSIX-only).
- `std::fs` - `read`, `read_to_string`, `write`, `read_dir`,
  `walk_dir`, `create_dir(_all)`, `remove_file/dir(_all)`,
  `remove_all`, `copy`, `rename`, `exists`, `is_file/dir/symlink`,
  `file_size`, `metadata`, `canonicalize`.
- `std::path` - pure manipulation (no I/O): `join`, `split`, `base`,
  `dir`, `ext`, `clean`, `is_absolute`, `has_prefix`, `matches`,
  `parent`, `file_name`, `stem`, `normalize`.
- `std::os` - `family()`, `arch()`; `write_file(path, &Vec<u8>)`
  (binary-safe) and `read_file(path) -> Result<Vec<u8>, _>` /
  `read_file_to_string`.
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
  `BTreeMap`. (Vec/HashMap method extras under §9.)
- `std::net` - `TcpListener::{bind, accept, local_addr, close}`,
  `TcpStream::{connect, read, read_to_string, write, close}`,
  `UdpSocket::{bind, send_to, recv_from, local_addr, close}`,
  `resolve` / `lookup` (DNS). `std::net::url` - parse + render +
  escape.
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
  HTTP/3 server + client (RFC 9114).
- `std::encoding::{json, base64, hex, binary}`. Every user struct
  gets generic serializer free functions called with a turbofish:
  `from_json::<Type>(text) -> Result<Type, _>` and
  `to_json::<Type>(value) -> Result<String, _>` (the single
  spelling - no `Type::from_json` methods). The decoder validates
  each field against its declared type with path-qualified errors;
  nested structs, `[T]`/`Vec<T>`/`[T; N]`/tuples/`Option<T>`/
  `HashMap<String, V>` walk recursively; a `json::Value` field
  passes through. `let user: User = from_json::<User>(&text)?` is
  canonical. Dynamic `json::parse` / `decode` / `render` +
  `json::as_i64/f64/str -> Option<T>` stay available for
  unknown-shape documents. Narrow int fields round-trip via `as`.
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
  `WaitGroup` (`new`/`add`/`done`/`wait`), `Map` (concurrent
  string→string: `set`/`get`/`delete`/`len`/`contains`/`keys`). For
  non-string payloads wrap a `HashMap` in `Mutex`.
- `std::time` - `Instant::{now, elapsed_ms}`, `Duration::{from_millis/secs/micros,
  as_millis/secs/micros}`, `sleep`, `now`, `now_nanos`,
  `monotonic_ms/nanos`, `since_ms`, `format_rfc3339`, `parse_rfc3339`.
  Channel timer: `after(d) -> Receiver` (one-shot) - drain with
  `while let` or use as a `select` timeout arm.
- `std::context` - cancellation, deadlines, `Context::background()`.
- `std::bytes` / `std::bufio` - binary buffers and buffered IO.
- `std::flag` - CLI flag parser; `flag::Cell<T>` auto-derefs at
  every value-context (comparisons, call args, `if`), explicit
  `*cell` still works.
- **Scalar `min` / `max` / `clamp`** - bare prelude functions, no
  import. `min(3, 7) == 3`, `clamp(15, 0, 10) == 10`. Vec-shaped
  `min(xs)` / `max(xs)` return `Option<T>`.
- `std::sort`, `std::math::rand` (deterministic RNG).
- `std::crypto::{rand, sha256, hmac, subtle}` - narrow, audited;
  `crypto::password` - Argon2id (`hash`, `verify`, `needs_rehash`,
  PHC strings).
- `std::jwt` - RFC 7519 sign/verify HS256/384/512, ES256, EdDSA,
  RS256/384/512 (verify): `sign_hs`/`verify_hs`,
  `sign_es256`/`verify_es256`, `sign_eddsa`/`verify_eddsa`.
- `std::metrics` - Prometheus `Counter`/`Gauge`/`Histogram` +
  `Registry`. `std::trace` - W3C trace-context + OTLP JSON exporter.
- `std::compress::{gzip, flate, zlib, zstd}` - byte-in/byte-out
  (zstd 1-22, default 3).
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
```

The optional `[project] entry` key names the entry source directly,
overriding the convention search; the resolved entry is the only file
allowed to carry top-level statements.

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

### Tier-divergence traps

The surface runs bit-identically across the bytecode VM (`gos run` /
`gos test`), the Cranelift JIT, and the LLVM AOT tier (`gos build`).
When you hit something that behaves differently across tiers it is a
bug - reduce it and check against `gos test` (interpreter) **and**
`gos build` (LLVM). One source-level rule remains:

- **Per-file test modules must have unique names.** Multiple
  `#[cfg(test)] mod tests` across bundled sibling files collide on
  `gos build`/`gos run` with `GR0003: name 'tests' defined multiple
  times` - name them `mod foo_tests`, `mod bar_tests`, etc.

## 15. Where to read more

- Language spec: `SPEC.md`. Style guide: `GUIDELINES.md`.
- Rendered docs: `docs_src/` → `site/`.
- Examples: `examples/` - start with `hello_world.gos`,
  `function_piping.gos`, `go_spawn.gos`, `concurrency.gos`.

## 16. When in doubt

Run it. `gos check` gives rustc-class diagnostics with source
excerpts and did-you-mean suggestions; `gos explain <CODE>` expands
any diagnostic code. The toolchain is your first debugger.
