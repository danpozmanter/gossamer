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

Write **clear, low-complexity, concise code.** Names earn their
length; helpers earn their existence. If a line reads cleanly the
first time through, leave it alone — don't dress it up.

Prefer these shapes when writing Gossamer:

- **Default to immutable.** `let x = …` is the first reach;
  `let mut x = …` is a deliberate exception only when the
  binding genuinely changes after construction. Build new
  values with expressions (`if`, `match`, `loop … break v`,
  iterator-style folds) before reaching for an accumulator
  pattern. Functions return the new value; callers shadow.
- **Compound-assign accumulators.** Use `+= -= *= /= %= &= |= ^= <<= >>=`.
  Never write `x = x + 1`; write `x += 1`. The compound forms
  parse, lower, and run on every tier — the longhand is a code
  smell that doubles the line length of every accumulator.
- **`if let` / `while let` for `Option` and single-variant matches.**
  `if let Some(n) = m.get(&k) { use(n) }` (one line) instead of
  `match m.get(&k) { Some(n) => use(n), None => () }` (four
  lines). `while let Some(v) = rx.recv()` is the canonical
  channel-drain shape.
- **Tuple destructuring at every binding site.**
  `let (a, b) = pair`, `for (k, v) in m.iter()`,
  `let (tx, rx) = channel()`. Skip the `pair.0` / `pair.1`
  reach-through.
- **`for x in xs` over collections — no `.iter()`, no `*x`.**
  `for n in [1, 2, 3] { sum += n }`. The binding is the value
  for `Copy` types and a borrow for the rest; the explicit
  `.iter()` + deref is legacy noise.
- **Bare integer indices — no `as usize` cast.**
  `arr[i]` works for `i: i64`. The runtime widens / bounds-checks
  for you. `arr[i as usize]` is a Rust habit that doesn't apply.
- **`Vec::swap` over the manual three-line dance.**
  `arr.swap(i, j)` mutates in place across every tier. Three-line
  `let t = arr[i]; arr[i] = arr[j]; arr[j] = t` is only justified
  in the hottest swap loops where the JIT-compiled register
  layout matters more than readability.
- **`m.inc(k)` / `m.inc(k, by)` for counter idioms.**
  `m.inc("apple")` (one call, one lock acquire) instead of
  `m.insert("apple", m.get_or("apple", 0) + 1)`. `m.or_insert(k, default)`
  for the get-or-fill pattern.
- **Recursive enums with `Box<T>`.** `enum List { Cons(i64, Box<List>), Nil }`
  works directly — `Box`, `Arc`, and `Rc` are transparent in a
  GC'd language, and the spelling matches Rust exactly. The
  bare `enum List { Cons(i64, List), Nil }` form works too;
  every variant payload is heap-shared.
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
- **`arena { ... }` for object graphs that die together.** Everything
  allocated inside the block is bump-allocated and freed wholesale at
  the block's exit (every exit path). Small-enum nodes drop their
  header (16-byte tree nodes); allocation is a pointer bump. Contract:
  nothing allocated inside may be referenced after the block — compute
  scalar/string summaries inside, keep survivors outside. Statement
  position only; arenas nest.
- **Bare numeric literals — always.** Write `0`, `200`, `1.5`,
  not `0i64`, `200i64`, `1.5f64`. Inference picks the type from
  the binding, the call site, or the return type, so the suffix
  is redundant in every well-typed program. Suffix only when the
  literal stands alone with *no* contextual hint at all
  (rare: top-level constants whose type cannot be inferred from
  the right-hand side). The same rule applies to `usize` —
  `arr[0]`, not `arr[0usize]`.
- **String literals are already `String`.** Don't write
  `"foo".to_string()` — the literal is the owned value. `&"foo"`
  borrows it where a `&String` / `&str` parameter is expected.
- **Macros only for formatted output.** `println!`,
  `format!`, `print!`, `eprintln!`, `eprint!`, `panic!` are
  the six macro entries — no others exist.

### Immutability default — concrete examples

Reach for `let mut` only when none of these shapes work.

```gossamer
// `if` / `match` are expressions — bind their result instead
// of mutating after the fact:
let label = if n < 0 { "negative" } else { "non-negative" }
let label = match shape {
    Shape::Circle(_) => "round",
    Shape::Rect { .. } => "boxy",
}

// For accumulator work, push the mutation into a small helper
// that returns the final value. Use `+=`, never `acc = acc + x`.
// The caller's binding stays `let`, not `let mut`:
fn sum(xs: &[i64]) -> i64 {
    let mut acc = 0
    for n in xs.iter() { acc += *n }
    acc
}

let total = sum(&xs)               // immutable at the call site
```

Heuristics: a `let mut` lives near a single update site (a
loop, a builder pattern, an in-place sort), inside a small
function whose return value is the new state. If the binding
is written from many places, the function probably wants to
be broken into smaller pieces that each return a fresh value.

### `if let` / `while let` — when to reach for them

```gossamer
// One-shot Option lookup — `if let` collapses a 4-line match.
if let Some(score) = scores.get(&name) {
    println!("{name}: {score}")
}

// Drain a channel until the producer hangs up.
while let Some(value) = rx.recv() {
    handle(value)
}

// Walk a linked-list / cause chain until the option exhausts.
let mut cursor = err.cause()
while let Some(inner) = cursor {
    println!("  caused by: {}", inner.message())
    cursor = inner.cause()
}

// Pattern-matching a single enum variant in flow.
if let Tree::Node(value, _, _) = node {
    println!("node = {value}")
}
```

Avoid `if let` when you genuinely need to handle every variant
— that's `match` with a guard, not `if let` with an `else`.

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
let n = 3 |> double |> add(10) |> clamp(0, 100)

// Discouraged — the same meaning, but the eye has to unwind.
let same = clamp(0, 100, add(10, double(3)))
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

// Idiomatic: small helper that owns the accumulator and
// hands back a fresh value. The caller binds it immutably.
fn sum(xs: &[i64]) -> i64 {
    let mut acc = 0
    for n in xs.iter() { acc = acc + *n }
    acc
}

fn main() {
    let total = sum(&[1, 2, 3])
    println!("total: {}", total)
}
```

## 5. Grammar essentials

- **Comments**: `//` single-line and `/* ... */` block are
  the only two forms — block comments do **not** nest, and
  there is no separate `///` / `//!` doc-comment syntax. A
  run of `//` lines immediately above an item (no blank line
  between) is its documentation; `gos doc` renders these and
  `gos test` runs fenced code inside them.
- **Semicolons** are optional at statement boundaries; one
  statement per line. A newline followed by a leading `&`,
  `*`, or `-` always starts a new statement (so `let s = expr\n&s
  |> ...` parses as two statements, not `expr & s`). For
  legitimate multi-line continuation of those three operators,
  put the operator at the end of the previous line
  (`let x = a -\n  b`) or parenthesize the expression.
- **Imports.** `use std::iter` for a single import; group with
  braces for several from the same module — `use std::{iter,
  os, strings}`. No trailing `;`. Alias an entry with
  `use std::collections::{HashMap as Map}`.
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
- **Integer literals** are bare by default: `1`, `255`, `0`.
  Inference picks the type from the binding, the call site, or
  the return type. Suffix only when no contextual hint exists
  (e.g. `1i32` standing alone in an expression with no other
  width signal). Unsuffixed literals default to `i64`.
- **Casts.** `x as i32` — whitelist-checked (numeric ↔ numeric,
  `bool` / `char` → integer, `u8` → `char`, same-type no-op).
  Every other `as` shape is a hard error (GT0005).
- **Patterns.** Wildcard `_`, literals, `name`, `mut name`,
  `Variant(…)`, `Struct { … }`, tuples `(a, b)`, ranges
  `1..=5`, or-patterns `a | b`, `@`-bindings `x @ 1..=3`,
  rest `..`. Guards: `Some(n) if n > 0 => …`. Patterns appear
  in `let`, `for`, function parameters, `match`, `if let`,
  and `while let`.
- **`if let` / `while let`** — sugar for the
  refutable-pattern-or-skip cases. `if let PAT = SCRUTINEE { … }
  else { … }` desugars to `match SCRUTINEE { PAT => …, _ => … }`;
  `while let PAT = SCRUTINEE { … }` to `loop { match SCRUTINEE
  { PAT => …, _ => break } }`. No new behavior, just shorter
  reading.

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
let greeting = "hello, " + &name
```

## 7. Error handling

Fallible functions return `Result<T, E>`. Propagate with `?` and
build / wrap / inspect errors through `std::errors`:

```gossamer
use std::errors
use std::fs

fn load_config(path: &String) -> Result<String, errors::Error> {
    fs::read_to_string(path)
        .map_err(|e| errors::wrap(e, format!("reading {}", path)))
}
```

- `errors::new(msg)` — build a free-standing error.
- `errors::newf(fmt, args…)` (0.7.0) — format-shaped error
  constructor, e.g. `errors::newf("status {}", code)`. Same
  `{}` placeholder rules as `format!`. Saves the surrounding
  `format!(...)` wrap on the dominant call shape.
- `errors::wrap(cause, msg)` — add a higher-level message.
- `errors::is(err, needle)` — walk the cause chain.
- `errors::chain(err)` — iterate the cause chain.
- `errors::join([err, err])` — combine several into one.

Idiomatic shape — fallible work returns `Result`, piped
through `result::map` for the ok-path and
`result::default_with` to handle the error in-line:

```gossamer
use std::{env, errors, fs, iter, result}

fn cat(f: &String) -> Result<(), errors::Error> {
    fs::read_to_string(f) |> result::map(|s| print!("{}", s))
}

fn main() {
    env::args() |> iter::for_each(|f| cat(&f) |> result::default_with(|e| eprintln!("{f}: {e}")))
}
```

`result::map(fn, r)` transforms `Ok(v)` via `fn`, leaving
`Err` untouched. `result::default_with(fn, r)` calls `fn`
on the error and returns `()`, consuming the result — the
data-last argument order lets both thread through `|>`.
`?` also works anywhere (including inside macro arguments
like `print!("{}", expr?)`) for sequential `let`-binding
style when that is clearer.

Panics are goroutine-scoped: a panic in a spawned goroutine ends
only that goroutine — the scheduler keeps running and the process
exits cleanly — while a panic on the main goroutine is fatal, as
in Rust. Reserve them for invariant violations, not recoverable
failure.

## 8. Concurrency

Goroutines via `go expr` (fire-and-forget). When you need the
result, `spawn(f)` runs `f` on a goroutine and returns a
`JoinHandle<T>`; `handle.join()` blocks for `Result<T, String>` —
`Ok(value)` on a normal return, or `Err(message)` if the goroutine
panicked. Closures may capture their environment.

```gossamer
let h = spawn(|| compute())
match h.join() {
    Ok(v) => println!("{}", v),
    Err(e) => eprintln!("worker failed: {}", e),
}
```

Typed channels via `std::sync::channel()`. `select { }` multiplexes
receives and sends:

```gossamer
use std::sync::channel
use std::time

fn main() {
    let pair = channel()
    let tx = pair.0
    let rx = pair.1

    go tx.send(10)
    go tx.send(20)
    go tx.send(30)

    time::sleep(50)

    let mut total = 0
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

## 8a. Closures and higher-order fns

Lambdas use `|param: T| body`. Captures from the enclosing scope
work as you'd expect (GC-managed, no `move` keyword).

For higher-order parameters, distinguish two callable types:

- `fn(args) -> ret` — raw code pointer, accepts only non-capturing
  items (bare functions, lifted lambdas with no captures).
- `Fn(args) -> ret` — callable trait, accepts both bare items
  and capturing closures. Fat pointer (env + code) under the
  hood; the conversion is implicit at the call site.

```gossamer
fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }

fn main() {
    let scale = 10
    let scaled = |y: i64| scale * y     // captures `scale`
    println!("{}", apply(scaled, 5))    // 50

    fn add_one(y: i64) -> i64 { y + 1 }
    println!("{}", apply(add_one, 41))  // 42 — bare fn coerces
}
```

Single trait variant — no `FnMut` / `FnOnce` distinction (the
borrow-style split Rust draws is unnecessary in a fully GC'd
world). `FnMut` / `FnOnce` parse but lower to the same
`Fn(_)` shape.

## 8b. Iterators

User code can declare its own iterator-shaped trait and let
`for x in ...` drive it. The for-loop desugars to
`{ let mut __iter = expr; loop { match (&mut __iter).next() {
Some(x) => body, None => break } } }` — any type that provides
`fn next(&mut self) -> Option<T>` is iterable.

```gossamer
struct Counter { next_value: i64, end: i64 }

trait Iterator {
    fn next(&mut self) -> Option<i64>
}

impl Iterator for Counter {
    fn next(&mut self) -> Option<i64> {
        if self.next_value < self.end {
            let v = self.next_value
            self.next_value = self.next_value + 1
            Some(v)
        } else {
            None
        }
    }
}

fn main() {
    let mut c = Counter { next_value: 0, end: 5 }
    let mut sum = 0
    for n in c { sum = sum + n }
    println!("sum 0..5 = {}", sum)  // sum 0..5 = 10
}
```

`std::iter::*` also exposes a lazy `Lazy` adapter wrapping
any Rust `Iterator`, with `map` / `filter` / `take` / `skip`
/ `step_by` adapters and `to_vec` / `sum` / `min` / `max` /
`count` / `any` / `all` terminals — chains stay allocation-
free until the terminal materialises a result.

## 9. Data structures

- `[T]` — growable array. Literal: `[1, 2, 3]`. Iterate with
  `for x in xs { … }` (no `.iter()`, no `*x`). Mutate in place
  with `xs.push(v)`, `xs.pop()`, `xs.swap(i, j)`, `xs.sort()`,
  `xs.sort_by(|a, b| …)`.
- `[T; N]` — fixed-size array. Literal: `[v; N]` (repeat) or
  `[a, b, c]` (annotated `[T; 3]`). Stack-allocatable, no
  growth — pick when the size is a compile-time constant.
- `(A, B, …)` — tuple. Field access via `.0`, `.1`, …, or
  destructure inline: `let (a, b) = pair`,
  `for (k, v) in m.iter()`.
- `struct Foo { x, y }` / `struct Pair(A, B)` — GC-managed
  value types.
- `enum E { A, B(Payload) }` — sum types, pattern-matched
  exhaustively. Recursive payloads work directly:
  `enum List { Cons(i64, Box<List>), Nil }`. `Box<T>` /
  `Arc<T>` / `Rc<T>` are transparent — every variant payload
  is heap-shared regardless of the spelling.
- `Option<T>` — `Some(T)` / `None`. Read with `if let`.
- `Result<T, E>` — `Ok(T)` / `Err(E)`. Propagate with `?`.
- `std::collections::{Vec, HashMap, HashSet, BTreeMap}` — the
  richer containers. `HashMap` extras worth knowing:
  `m.inc(k)` / `m.inc(k, by)` (counter-style increment),
  `m.or_insert(k, default)` (get-or-fill),
  `m.iter()` (yields `[(K, V)]` for direct destructuring).

## 10. The `gos` toolchain

Every subcommand takes a `.gos` file or a project directory.
Bare `gos` drops into the REPL.

| Command | Purpose |
|---------|---------|
| `gos check FILE` | Parse + resolve + typecheck + exhaustiveness. |
| `gos run FILE` | Register-based bytecode VM. The walker is gone as a user-facing mode; if the VM hits an HIR shape it doesn't lower yet, it falls back internally — never user-selectable. |
| `gos build FILE` | Native build via LLVM (`opt -O0 \| llc -O0`) + system linker. |
| `gos build --release FILE` | Native build via LLVM (`opt -O3 \| llc -O3 -mcpu=native`) + system linker. |
| `gos test PATH` | Discover and run `#[test]` functions. `--coverage <path>` (lcov), `--parallel N` / `--serial`, `--format junit`, `--tier-parity --report=status`. |
| `gos bench PATH` | Discover and time `#[bench]` functions. |
| `gos fmt [--check] FILE` | Rewrite canonically. |
| `gos doc FILE` | Print item listing + doc comments. |
| `gos lint [--deny-warnings] PATH` | Run the lint suite. |
| `gos explain CODE` | Long-form rationale for a diagnostic code. |
| `gos watch --command CMD PATH` | Re-run on file change. |
| `gos clean [--vendor] [--dry-run]` | Remove build artifacts (`target/`), the per-project `.gos-cache` IR-object cache, and the frontend cache; `--vendor` also drops `vendor/`. |
| `gos new ID --path DIR` | Scaffold a project. |
| `gos add SPEC` / `remove ID` / `tidy` / `fetch` / `vendor` | Package manager. |
| `gos publish` / `yank` / `login` / `logout` / `owner` | Registry workflow (0.8.0). Credentials in `~/.config/gossamer/credentials.toml`, Ed25519-signed tarballs, `tarball_sha256` pinned in the lockfile. |
| `gos feature-status` | List or `--check` the feature-status registry. `--status shipped\|experimental\|planned\|removed`, `--format table\|json\|markdown`. |

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
        let total = super::add(2, 3)
        assert(total == 5)
    }
}
```

Doc-tests: fenced code inside a `//` doc-comment block (a
run of `//` lines directly above an item) is compiled and
executed by `gos test`. Mark non-runnable fences as
` ```text `.

## 12. Standard library surface

- `std::fmt` — `Display`, `Debug`.
- `std::io` — `Read`, `Write`, buffered wrappers, `stdin` / `stdout`.
- `std::env` — process environment and CLI args:
  `args`, `program_name`, `var`, `set_var`, `unset_var`,
  `current_dir`, `set_current_dir`, `home_dir`, `temp_dir`.
- `std::process` — child processes and exit:
  `Command`, `Output`, `Stdio`, `Child`, `ExitStatus`,
  `run`, `spawn`, `kill`, `exit`, `id`, `abort`. **0.8.0**:
  `Pipeline` for stdout→stdin chaining (`pipeline_run`),
  `Signal` enum, `signal(pid, sig)`, `kill_group(pgid, sig)`,
  `wait_timeout(child, ms)` — all wired through the compiled
  tier via `gos_rt_exec_*` shims (POSIX-only).
- `std::fs` — filesystem (Rust-style):
  `read`, `read_to_string`, `write`, `read_dir`, `walk_dir`,
  `create_dir`, `create_dir_all`, `remove_file`, `remove_dir`,
  `remove_dir_all`, `remove_all`, `copy`, `rename`, `exists`,
  `is_file`, `is_dir`, `is_symlink`, `file_size`, `metadata`,
  `canonicalize`, `glob`, `eval_symlinks`.
- `std::path` — pure path manipulation (no I/O):
  `join`, `split`, `base`, `dir`, `ext`, `clean`,
  `is_absolute`, `has_prefix`, `matches`. `path::native` for
  backslash-style paths on Windows.
- `std::os` — OS identity + deprecated re-exports of env/process/fs
  for one release: `family()`, `arch()`.
- `std::strings` — `split`, `splitn`, `split_whitespace`, `trim`,
  `trim_start`, `trim_end`, `contains`, `find`, `rfind`,
  `replace`, `replacen`, `to_lower`, `to_upper`, `starts_with`,
  `ends_with`, `repeat`, `lines`, `join`, `strip_prefix`,
  `strip_suffix`, `pad_left`, `pad_right`. **0.7.0 additions**
  (also available as `String` methods on a String receiver):
  `split_once(sep) -> Option<(String, String)>`,
  `rsplit_once(sep) -> Option<(String, String)>`,
  `count(needle) -> i64`, `strip_chars(cutset)` / `lstrip_chars` /
  `rstrip_chars`, `zfill(width)`, `center(width, pad_char)`, and
  `slice(start, end) -> Result<String, errors::Error>` — the
  non-panicking byte-range slice. Use `String::slice(s, a, b)?` to
  propagate. `s.byte_at(i) -> i64` returns the UTF-8 byte at index `i`
  (0 if out of range), the constant-time primitive for byte-level
  scanners/parsers.
- `std::strconv` — `parse_int`, `parse_i64`, `parse_u64`,
  `parse_float`, `parse_f64`, `parse_bool`, `format_int`,
  `format_i64`, `format_float`, `format_f64`, `itoa`, `atoi`.
- `std::path` — `parent`, `file_name`, `stem`, `ext`,
  `is_absolute`, `normalize` (in addition to `join` / `walk`).
- `std::utf8` — `count_runes`, `rune_count`, `rune_count_in_string`,
  `rune_len`, `is_valid`, `valid_rune`, `valid_string`,
  `full_rune` / `full_rune_in_string`, `rune_start`,
  `decode_rune` / `decode_last_rune` / `decode_first` (and
  the `_in_string` variants), `encode_rune` / `append_rune`.
- `std::unicode` — **full Unicode 16 surface** (0.8.0). General-category
  predicates: `is_letter`, `is_digit` (Nd), `is_number` (Nd|Nl|No),
  `is_space` (Z* + ASCII whitespace), `is_upper` / `is_lower` /
  `is_title`, `is_punct` (P*), `is_symbol` (S*), `is_mark` (M*),
  `is_print`, `is_graphic`, `is_control`, `is_assigned`,
  `combining_class`. Casing: `to_upper` / `to_lower` / `to_title` /
  `simple_fold` for single runes; `to_upper_str` / `to_lower_str` /
  `fold_case` for whole strings (handles ß → SS, Σ → σ, etc.).
  Normalization: `nfc`, `nfd`, `nfkc`, `nfkd`, plus `is_nfc` /
  `is_nfd` / `is_nfkc` / `is_nfkd`. Segmentation (UAX #29):
  `graphemes(s) -> Vec<String>`, `grapheme_count(s) -> i64`,
  `words(s)` / `word_bounds(s)` / `word_count(s)`,
  `sentences(s)` / `sentence_count(s)`. All entries work on
  every tier (VM / Cranelift / LLVM) via `gos_rt_unicode_*`
  C-ABI shims backed by the `unicode-properties`,
  `unicode-normalization`, and `unicode-segmentation` crates.
  **Identifier rules** also follow Unicode: `let café = 1`,
  `let π = 3.14`, `let 名前 = "x"` all parse via UAX #31
  `XID_Start` / `XID_Continue` (matches Rust 2024).
- `std::collections` — `Vec`, `HashMap`, `HashSet` (real set
  with `insert`, `remove`, `contains`, `len`, `is_empty`,
  `clear`, `to_vec`, `iter`), `BTreeMap`. **0.7.0 Vec method
  additions:** `contains(&v) -> bool` (also works on `[T]` /
  `[T; N]`), `index_of(&v) -> Option<i64>`,
  `count_of(&v) -> i64`, `first() -> Option<T>`,
  `last() -> Option<T>`, `reversed() -> Vec<T>` (non-mutating;
  pair with the existing `reverse()` for in-place). The safe
  Result-returning sub-range slicer is
  `xs.slice(start, end) -> Result<Vec<T>, errors::Error>` —
  inverted or out-of-range bounds return Err rather than
  panicking. The Result-returning mutation entries
  `Vec::insert(xs, i, v) -> Result<Vec<T>, errors::Error>` and
  `Vec::remove(xs, i) -> Result<T, errors::Error>` are exposed
  as qualified free functions; the legacy `xs.insert(i, v)` /
  `xs.remove(i)` method-call shape keeps its silent
  in-place semantics. **0.7.0 HashMap additions:** `keys()` and
  `values()` return `Vec<K>` / `Vec<V>` directly;
  `HashMap::pop(m, k) -> Option<V>` removes and returns the
  previous value Python-style.
- `std::net` — `TcpListener::{bind, accept, local_addr, close}`,
  `TcpStream::{connect, read, read_to_string, write, close}`,
  `UdpSocket::{bind, send_to, recv_from, local_addr, close}`,
  `net::resolve` / `net::lookup` for DNS.
- `std::net::url` — URL parse + render + escape.
- `std::http` — `Method`, `StatusCode`, `Headers`, `Request`,
  `Response`, `Handler`, `serve`. Client surface:
  `Client { get, post, put, options, delete, head, request, stream }`
  plus free wrappers `http::get(url, headers)`,
  `http::post(url, body, content_type)`,
  `http::put(url, body, content_type)`,
  `http::options(url, headers)`,
  `http::delete(url, body, headers)`, `http::head(url, headers)`,
  `http::request(method, url, body, headers)`, and
  `http::stream(method, url, body, headers) -> ResponseStream`
  whose `next_line()` reads SSE / chunked bodies one line at a
  time. All method-string entry points accept
  `"GET"`/`"POST"`/`"PUT"`/`"DELETE"`/`"PATCH"`/`"HEAD"`/`"OPTIONS"`
  case-insensitively; unknown methods return `Err(transport)`.
- `std::http` server stack (**0.8.0**):
  - `http::cookie` — RFC 6265 `Cookie` / `CookieBuilder`,
    `SameSite`, `parse_cookie_header`, `parse_set_cookie`.
  - `http::csrf` — double-submit cookie + Origin/Referer check:
    `issue_token`, `verify_token`, `extract_token`,
    `origin_allowed`, `check`, `attach_cookie`, `RouteAuth`.
  - `http::form` — `application/x-www-form-urlencoded` parse +
    build.
  - `http::multipart` — streaming RFC 7578 with `parse_boundary`,
    `parse_bytes`, `parse<R: Read>`, `Part`, `PartData`, `Form`.
  - `http::query` — typed `Query` wrapper over URL query strings.
  - `http::session` — signed-cookie sessions: `SessionConfig`,
    `Session`, `SessionStore` trait, `SignedCookieStore`,
    `with_session`.
  - `http::state` — `AppState` typemap + `State<T>(Arc<T>)` DI
    for handlers.
  - `http::health` — `Probe` trait + `Health` aggregator,
    `always_ok` / `always_fail` / `tcp_probe`.
  - `http::middleware` — `body_limit`, `timeout`, `hsts`,
    `security_headers`, `cache_control`, `etag`, `bearer_auth`,
    `rate_limit`, `compress_gzip`, `safe_defaults`, plus the
    existing `logger`, `recoverer`, `request_id`, `cors`,
    `basic_auth`.
  - HTTP/2 server push + trailers: `PushOptions`, `PushStream`,
    `ResponseWriter::push_promise`, `ResponseWriter::write_trailers`,
    `Request::trailers`.
- `std::encoding::{json, base64, hex, binary}`. Every user struct
  gets a pair of generic serializer free functions, called with a
  turbofish type argument:
  `from_json::<Type>(text) -> Result<Type, errors::Error>` and
  `to_json::<Type>(value) -> Result<String, errors::Error>`. This is
  the single spelling — there are no `Type::from_json` methods. The
  decoder checks each field against its declared type and rejects
  type mismatches and missing required fields with path-qualified
  errors. Nested structs, `[T]` / `Vec<T>` / `[T; N]` / tuples /
  `Option<T>` / `HashMap<String, V>` walk recursively; a
  `json::Value` field passes through untouched.
  `let user: User = from_json::<User>(&text)?` is the canonical
  shape. The dynamic `json::parse` / `json::decode` /
  `json::render` surface stays available for documents whose shape
  isn't known at compile time (`json::as_i64` / `as_f64` / `as_str`
  return `Option<T>`). The same synth also emits
  `from_yaml::<T>` / `to_yaml::<T>` (piggybacks on `to_json` +
  `yaml::from_json`) so `from_yaml::<Config>(&text)?` works against
  the same struct definition. Narrow integer fields
  (`i8`/`i16`/`i32`/`u8`/`u16`/`u32`) are accepted and round-trip
  through the `as <width>` cast at the JSON boundary.
- `std::encoding::yaml` — YAML 1.2 parse/encode plus
  `yaml::to_json(text)` / `yaml::from_json(text)` text-shape
  converters that mirror `toml::to_json` / `from_json`. The
  auto-derived `from_yaml::<T>` / `to_yaml::<T>` functions on every
  user struct compose these with the JSON pair.
- `std::sync` — `Mutex`, `RwLock`, atomics, `channel`, `Once`,
  `WaitGroup` (`new`, `add`, `done`, `wait`), and `Map` (a
  concurrent string-keyed string-value map; `set`/`get`/`delete`/
  `len`/`contains`/`keys`). For non-string payloads, wrap a
  `HashMap` in `Mutex` directly — `sync::Map` is the optimized
  shape for caches and feature-flag tables.
- `std::os::write_file(path, &Vec<u8>)` preserves binary bytes
  (images, gzip, embedded NULs); the c-string-shaped string
  overload still works for text writes. `std::os::read_file(path)`
  returns `Result<Vec<u8>, errors::Error>` — pair with
  `os::read_file_to_string` for UTF-8 text.
- `std::http::Response.raw_bytes` exposes the response body as
  `Vec<u8>` for binary downloads (counterpart to the
  UTF-8-lossy `.body` field).
- `std::time` — `Instant::{now, elapsed_ms}`, `Duration::{from_millis,
  from_secs, from_micros, as_millis, as_secs, as_micros}`,
  `sleep`, `now`, `now_nanos`, `monotonic_ms`, `monotonic_nanos`,
  `since_ms`, `format_rfc3339`, `parse_rfc3339`.
- `std::context` — cancellation, deadlines, `Context::background()`.
- `std::bytes` / `std::bufio` — binary buffers and buffered IO.
- `std::errors` — wrap / chain / join.
- `std::flag` — CLI flag parser. **0.7.0:** `flag::Cell<T>`
  auto-derefs at every value-context expression on all three
  tiers (VM, cranelift, LLVM): binary comparisons (`flags.output
  == "text"`), function-call arguments (`get_comic(flags.number)`),
  conditional positions (`if flags.verbose { … }`), typed-i64 /
  f64 register unboxes. The explicit `*flags.output` still works
  if the user wants the resolved value as a local binding.
- **Scalar `min` / `max` / `clamp`** (0.7.0) — bare prelude
  functions, no import needed. `min(3, 7) == 3`,
  `clamp(15, 0, 10) == 10`. The Vec-shaped `min(xs)` /
  `max(xs)` fallback returns `Option<T>` for callers already
  on the collection form.
- `std::sort` / `std::utf8` / `std::path` / `std::fs`.
- `std::math::rand` — deterministic RNG.
- `std::crypto::{rand, sha256, hmac, subtle}` — narrow, audited.
  **0.8.0**: `crypto::password` — Argon2id facade (`hash`,
  `verify`, `needs_rehash`) producing PHC strings.
- `std::jwt` (**0.8.0**) — RFC 7519 sign + verify for HS256/384/512,
  ES256, and EdDSA: `Alg`, `Header`, `Claims`, `VerifyOpts`,
  `sign_hs` / `verify_hs`, `sign_es256` / `verify_es256`,
  `sign_eddsa` / `verify_eddsa`.
- `std::lifecycle` (**0.8.0**) — graceful-shutdown hooks, signal
  handling, sd_notify.
- `std::validate` (**0.8.0**) — `Validate` trait plus `FieldError`
  / `Errors` for form-style field validation.
- `std::slog` — structured logging.
- `std::runtime` — scheduler + memory knobs: `collect_cycles()`,
  `arena_push()` / `arena_pop()` (prefer the `arena {}` block).
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
use std::env
use std::flag

fn main() -> Result<(), flag::Error> {
    let mut fs = flag::Set::new("myapp")
    let port = fs.uint("port", 8080, "listen port")
    let verbose = fs.bool("verbose", false, "chatty output")
    let _ = fs.parse(env::args())?

    // 0.7.0: flag cells auto-deref at value contexts —
    // `verbose` and `port` work bare in `if`, comparisons,
    // and function-call args without the leading `*`.
    if verbose {
        println!("starting on port {}", port)
    }
    Ok(())
}
```

### HTTP server with method + path routing

A real service routes per (method, path) and keeps each handler
a one-job free function. Lift dispatch into one `App::serve`
that matches and forwards — never inline the response shape in
the dispatcher. The full pattern is in `examples/web_server.gos`.

```gossamer
use std::http

fn health(_r: http::Request) -> Result<http::Response, http::Error> {
    Ok(http::Response::text(200, "ok"))
}

fn list_users(_r: http::Request) -> Result<http::Response, http::Error> {
    Ok(http::Response::json(200, "[{\"id\":1,\"name\":\"ada\"}]"))
}

fn create_user(r: http::Request) -> Result<http::Response, http::Error> {
    Ok(http::Response::json(201, format!("{{\"body\":\"{}\"}}", r.body)))
}

struct App { }

impl http::Handler for App {
    fn serve(&self, r: http::Request) -> Result<http::Response, http::Error> {
        let method = &r.method
        let path = r.path()
        if path == "/health" { return health(r) }
        if path == "/users" {
            if method == "POST" { return create_user(r) }
            return list_users(r)
        }
        Ok(http::Response::text(404, "not found"))
    }
}

fn main() -> Result<(), http::Error> {
    http::serve("0.0.0.0:8080", App { })
}
```

Mirrors Go's `http.ServeMux` ergonomics: handlers are free
functions with the standard signature, the dispatcher is one
match per (method, path), exact match by default with
`path.starts_with` giving prefix routes when you need them.

## 15. Current gaps (pre-1.0.0)

- `+` on `String` copies; for heavy assembly use
  `std::bytes::Builder` or a `mut String` with `+=`.
- Method dispatch is name-global in places. Qualified path
  calls (`Point::origin()`) always work; method-style may
  collide across types until the resolver tightens.
- The scheduler is cooperative and unbuffered today.
  Channels work under `gos run`; `gos build` for programs
  that create channels is not yet wired — it will bail with
  a clear message. `go` spawn by itself builds natively.
- `env::args()` can return empty under some codegen paths —
  prefer `std::flag` with explicit defaults.
- `arr.sort_by(|a, b| …)` works in the bytecode VM. The
  cranelift JIT auto-skips bodies that call it (the
  closure-callback ABI isn't wired through native code yet)
  and the body runs on the bytecode VM instead, so sort
  behaviour is correct everywhere — just not JIT-fast.

## 16. Style rules

- **Default to immutable bindings.** `let` first, `let mut`
  only when a single named accumulator is the clearest shape.
  Express transformations with `if` / `match` / `fold` /
  `map` / `collect`; mutate locally and return the final
  value rather than threading mutation through callers.
- **Compound assignment everywhere.** `x += 1`, never `x = x + 1`.
  Same for `-= *= /= %= &= |= ^= <<= >>=`.
- **`if let` / `while let` for refutable patterns.** Reach for
  `match` only when you need every variant; otherwise the
  one-line `if let Some(n) = …` form is strictly better.
- **Tuple destructuring at the binding.** `let (a, b) = pair`,
  `for (k, v) in m.iter()`, `let (tx, rx) = channel()` — no
  `pair.0` / `pair.1` reach-through unless the tuple is
  threaded somewhere else first.
- **`for x in xs` over `for x in xs.iter()`.** No `.iter()`,
  no `*x`. The binding is the value (`Copy`) or a borrow
  (others).
- **No `as usize` on indices.** `arr[i]` works for `i: i64`.
- **Use the helpers.** `arr.swap(i, j)`, `m.inc(k)`,
  `m.or_insert(k, default)` — never the longhand.
- **Clear, low-complexity, concise.** Plain reads beat clever
  ones. If a helper, type, or comment doesn't earn its space,
  drop it.
- **No emojis.** Source, comments, commits, docs — all plain.
- **No TODO / FIXME** committed; open an issue.
- **Doc every `pub` item** with a single-line `//` directly
  above it (no blank line between); don't narrate
  self-evident code. Gossamer has no `///` / `//!` form.
- **Pipe aggressively** — if a value flows through more
  than one call, use `|>`.
- **`iter::*` over hand-rolled `for` loops for transformations.**
  `xs |> iter::for_each(handle)` instead of
  `for x in xs { handle(x) }` when the body is a single call.
  `let total = xs |> iter::sum_by(|n| n*n)` instead of
  `let mut total=0; for n in xs { total += n*n }`. The
  combinators (`map`, `filter`, `filter_map`, `fold`, `reduce`,
  `for_each`, `find`, `group_by`, `partition`, …) live as free
  functions in `std::iter` with data-last argument order so they
  thread through `|>`. Keep `for` for side-effects with
  complex state, early-return shapes, or `break`/`continue`
  flows.
- **`option::*` / `result::*` for in-pipeline chaining.**
  `parse(s) |> result::map(render) |> result::default("")`
  instead of a `match` with two arms when each arm is an
  extract-or-default. `?` is still the right tool for
  short-circuit propagation; the combinators are for transforming
  values mid-chain.
- **Free functions in `std::iter`, not methods on collections.**
  `Vec<T>` / `HashMap` / `HashSet` do not carry `.map` /
  `.filter` / `.fold` methods. The mutating helpers
  (`xs.push`, `xs.sort`, `m.inc`, `m.or_insert`) stay as
  methods because they operate by side-effect on the receiver.
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
