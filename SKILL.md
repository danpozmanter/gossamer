# Gossamer - Skill Card

Drop this file into a model's context to teach it idiomatic Gossamer.
Self-contained. For anything not covered here, lean on the toolchain
(section 2) instead of guessing.

## 1. What Gossamer is

A goroutine-powered, fast-compiling language with automatic,
deterministic memory management (reference counting with cycle
collection, plus `arena { }` regions - no borrow checker, no
lifetimes, no tracing-GC pauses). Syntax is Rust-flavoured; the
runtime is Go-shaped (goroutines, channels). Source files end in
`.gos`, the toolchain binary is `gos`, projects carry a
`project.toml` manifest. Pre-1.0.0: the surface is stable to write
against, and every feature ships across all three tiers (bytecode
VM, in-process JIT, LLVM AOT).

## 2. Lean on the toolchain

Run code early and often - the toolchain is the reference, not this
card:

- `gos mcp` (stdio MCP server, e.g. `claude mcp add gossamer -- gos
  mcp`) exposes `check` (structured diagnostics), `run`, `build`,
  `test`, `fmt`, `doc`, `explain`, and semantic navigation (`hover`,
  `definition`, `references`, `workspace_symbols`). Prefer these to
  memorized API detail: `hover` answers "what is this and its type",
  `check` validates a draft, `explain CODE` expands any diagnostic,
  `doc` lists a file's items. This card ships as its
  `gossamer://skill-card` resource.
- Without MCP: `gos check FILE` (rustc-class diagnostics with
  did-you-mean), `gos run FILE`, `gos explain CODE`, `gos doc FILE`.
- `gos check` is necessary, not sufficient - semantics are proven by
  `gos run`, and compiled behavior by `gos build`.

## 3. Cheat sheet

```gossamer
use std::io

const PI: f64 = 3.14159

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

## 4. Idioms

Write clear, low-complexity, concise code.

- **Default to immutable.** `let` first; `let mut` only for a binding
  that genuinely changes, kept near a single update site. `if` /
  `match` / `loop ... break v` are expressions - bind their result.
- **Compound-assign accumulators**: `x += 1`, never `x = x + 1`.
- **`if let` / `while let`** for `Option` and single-variant matches;
  `while let Some(v) = rx.recv()` is the canonical channel drain.
  Let-chains: `if let Some(x) = a && let Some(y) = b && x > 0 { .. }`
  (`&&`-only; earlier binds visible later).
- **Tuple destructuring everywhere**: `let (a, b) = pair`, `for (k, v)
  in m.iter()`, `let (tx, rx) = channel()`.
- **`for x in xs`** over collections - no `.iter()`, no `*x`.
- **Bare integer indices** - `arr[i]` takes `i64`, no `as usize`. A
  scalar-element index outside `[0, len)` yields the element's ZERO
  value (guard with `len()` when absence must differ from zero); an
  aggregate-element OOB access panics. Same on every tier.
- **`m.inc(k)` / `m.inc(k, by)`** for counters; `m.or_insert(k,
  default)` for get-or-fill (`m.or_insert(k, d).method(args)` writes
  the mutation back into the stored value). `arr.swap(i, j)`.
- **Method-form sequence combinators** - no import, ranges included:
  `xs.map(f)`, `filter`, `sum`, `min`/`max` (`Option<T>`), `count`,
  `any`/`all`, `find`/`position`, `max_by_key`/`min_by_key`, `fold`,
  `take`, `step_by`; `(1..5).map(|i| i * i).sum()`. Data-last `iter::`
  free forms exist for `|>` pipelines. `xs.join(sep)` Display-joins
  scalar/String sequences.
- **`s.to_i64()` / `to_f64()` / `to_bool()`** - strict full-string
  parses returning `Option<T>`:
  `env::args().first().unwrap_or("8").to_i64().unwrap_or(8)`.
- **`[v; n]` with runtime `n` builds a Vec** - never write a push loop
  for a constant fill.
- **Collection constructors infer**: `let mut m = HashMap::new()`,
  `let mut xs = []` ground from first use.
- **Byte reads**: `s[i]` is the byte as `i64`; compare with byte
  literals (`s[i] >= b'0'`), render with `s[i] as char`. Prefer this
  over per-byte `substring`.
- **Format captures walk field paths**: `println!("{name}:
  {a.balance} {t.0} {o.inner.hits} {a.balance:>8} {f.0:.2}")`.
- **Range binds looser than arithmetic, tighter than `|>`**:
  `i * i..n` is `(i * i)..n`; `0..n |> iter::sum` pipes the range.
- **Recursive enums work directly**: `enum List { Cons(i64,
  Box<List>), Nil }`; `Box`/`Arc`/`Rc` are transparent (bare
  `Cons(i64, List)` works too).
- **Structs, enums, arrays, Vecs, tuples compare structurally** - no
  derive; lexicographic by declaration order / variant rank. A user
  `impl` of `eq`/`cmp` overrides.
- **Derivable**: `#[derive(Debug, Default, PartialEq, Eq, PartialOrd,
  Ord)]` (enums too; `#[default]` picks the variant). `Clone`,
  `Hash`, `Copy`, `Display`, `Serialize` are NOT derivable (GT0025):
  copy, hashing, and serde are automatic; conversions and operators
  are written `impl Trait for T` - `From`/`TryFrom` (used by
  `x.into()`/`x.try_into()`), `Add` `Sub` `Mul` `Div` `Rem` `Neg`
  `Index` `BitOr` `BitAnd` `BitXor` `Shl` `Shr`.
- **Labeled loops**: `'outer: for .. { break 'outer }`.
- **`defer expr`** - runs when control leaves the enclosing `{ }` by
  any path, LIFO; per-iteration in a loop body.
- **`let PAT = expr else { ... }`** - the else block must diverge.
- **Plain functions for free-standing logic**; `impl` only when state
  is genuinely tied to a type.
- **`Result<T, E>` + `?`** for fallibility; panic only for invariant
  violations. Exhaustive `match` - no `_ =>` unless every unmatched
  case genuinely means the same thing.
- **Goroutines + channels** for async work; `sync::Mutex` only when
  shared memory is simpler.
- **`arena { ... }`** for object graphs that die together:
  bump-allocated, freed wholesale at every exit. Nothing allocated
  inside may be referenced after the block - statically enforced
  (`GM0003`). Statement position only; nests.
- **Bare numeric literals** - `0`, `1.5`, never `0i64`; suffix only
  with no contextual hint. String literals are already `String` - no
  `.to_string()`; `&"foo"` borrows where `&String`/`&str` is expected.
- **Fixed macro set** - everything else `name!(..)` is a parse error
  (no user macros): `println!` `print!` `eprintln!` `eprint!`
  `format!` `panic!` (Rust `{}` / `{name}` / `{:spec}` formatting:
  width/align/fill `{:*>8}`, zero-pad `{:08}`, radix `{:x}` `{:b}`
  `{:o}`, precision `{:.2}`); `matches!`, `todo!`, `unimplemented!`,
  `unreachable!`, `dbg!`; build-time `regex!` / `sql!` (validate the
  literal) and `codegen!` (splice a `comptime fn`'s String).

## 5. The `|>` forward-pipe

Prefer `|>` when a value flows through two or more transforms.
Left-associative, very low precedence.

- `x |> f` is `f(x)`; `x |> f(a, b)` puts `x` in the LAST slot:
  `f(a, b, x)`; same for `x |> recv.m(a)`.
- `_` makes the piped value the RECEIVER: `x |> _.m(a)` is `x.m(a)`;
  bare `s |> _.trim |> _.to_uppercase` chains nullary methods; `_.0`,
  `_[i]`, and bare `_` (identity) work.
- Closure steps thread the value into the last slot too.

```gossamer
let n = 3 |> double |> add(10) |> clamp(0, 100)
```

## 6. Grammar essentials

- **Comments**: `//` and non-nesting `/* */` only. A run of `//`
  lines directly above an item is its documentation; `gos test` runs
  fenced code inside doc comments (mark non-runnable fences
  ` ```text `).
- **Semicolons optional**; one statement per line. A newline followed
  by leading `&`, `*`, or `-` starts a NEW statement - for multi-line
  continuation, end the previous line with the operator or
  parenthesize.
- **Imports**: `use std::{iter, os, strings}`, alias via `{HashMap as
  Map}`; always spell the full path (`std::encoding::json`, not
  `std::json`) - paths validate against the std manifest (GR0005).
  Cross-module: `super::item`, `crate::path::item`, `self::child::item`.
- **Entry file may omit `fn main`**: top-level statements become the
  implicit main (only the entry file; `?` there makes it return
  `Result<(), errors::Error>`; exit code via `process::exit(n)`).
- **Generics**: `fn f<T: Trait>(x: &T)` monomorphises per call site
  on every tier (single bound, struct-typed params; no `dyn Trait`).
  Generic structs `struct Wrapper<T>` + `impl<T>` work. Const-generic
  array length `fn sum<const N: usize>(xs: [i64; N])` is inferred
  from the argument (not usable as a bare value or repeat count).
- **References**: `&x` shared, `&mut x` exclusive - aliasing intent
  only; the runtime owns memory. No lifetimes, no borrow checker.
- **Types**: `bool char i8..i64 u8..u64 isize usize f32 f64 String
  [T] [T; N] (A, B) Option<T> Result<T, E> &T` + user types. `i128`
  / `u128` are rejected (GT0014). Transparent `type Id = i64` /
  `type Pair<A> = (A, A)` aliases substitute everywhere (cycle =
  GT0024).
- **Casts**: `x as T` is whitelist-checked (numeric<->numeric,
  bool/char->int, u8->char). Int->narrow-int masks at width (`300 as
  u8 == 44`); float->int truncates toward zero and saturates at i64
  width, NO narrow mask (`300.7 as u8 == 300`, NaN -> 0).
- **Patterns**: literals, `_`, `name`, `mut name`, variants, structs,
  tuples, ranges (`1..=5`, `1..5`, open-ended `lo..` / `..=hi`;
  range arms still need a `_` arm - opaque to exhaustiveness),
  or-patterns, `@`-bindings, rest `..` (slice head/tail: `if let
  [first, ..rest] = xs`), guards. Irrefutable `let` destructuring
  covers structs (with renames), nested enums/tuple-structs, and
  or-patterns binding the same names.
- **Byte literals**: `b'A'` is a `u8`.
- Enums cap at 256 variants (GT0012).

## 7. Comptime (the metaprogramming story)

Zig-style `comptime`, not macros: `comptime { }` blocks, `comptime
fn` calls, and `comptime` params run on the bytecode VM during
compilation and fold to literals, so every tier compiles the same
constant. `typeInfo::<T>()` reflects fields; a plain `for (name, ty)
in typeInfo::<T>()` loop unrolls per field at compile time
(`field_of(v, name)` projects) - the basis for reflection-driven
serializers specialized per turbofish call site. `codegen!` splices
a `comptime fn`-built `String` as source.

## 8. Error handling

Fallible functions return `Result<T, E>`; propagate with `?` (also
works on `Option` inside Option-returning functions, auto-converts
errors through `From`, and works inside macro args).

```gossamer
use std::{errors, fs}

fn load(path: &String) -> Result<String, errors::Error> {
    fs::read_to_string(path)
        .map_err(|e| errors::wrap(e, format!("reading {}", path)))
}
```

`errors::new(msg)` / `newf(fmt, ..)` / `wrap(cause, msg)` /
`is(err, needle)` / `join([..])`; walk chains with `err.cause()`.
`{}` on a wrapped error prints the colon-joined chain;
`.message()` is the top message only. Ok-path piping:
`fs::read_to_string(f) |> result::map(|s| print!("{s}"))
|> result::unwrap_or_else(|e| eprintln!("{e}"))`.

Panics are goroutine-scoped: a spawned goroutine's panic ends only
that goroutine; a main-goroutine panic is fatal (exit 101). Integer
divide/modulo by zero panics (GX0005); `i64::MIN / -1` wraps. Deep
recursion is bounded: the VM/JIT reports GX0008 and native builds use
the installed stack guard (`stack overflow ... aborting`) rather than
silently corrupting memory.

## 9. Concurrency

`go expr` is fire-and-forget; `spawn(f)` returns a `JoinHandle<T>`
(`handle.join()` -> `Result<T, String>`). Typed channels: `recv()`
blocks until a value or every sender is gone; producers `close()`.

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

`select { x = rx_a.recv() => .., default => .. }` multiplexes (arms
poll in source order; `default` makes it non-blocking). One-shot
timer: `time::after(d) -> Receiver` as a select timeout arm.
`std::sync` also has `Mutex`, `RwLock`, atomics, `Once`, `WaitGroup`,
`Barrier`, and `Map` (concurrent string->string); `std::thread` is
real OS threads.

**Closures**: `|x: T| body`; capture is automatic (no `move`).
Use `Fn(args) -> ret` for callback parameters. Plain `fn(args) -> ret`
is a raw pointer shape; named function item coercion is not implemented
(no FnMut/FnOnce distinction in practice).

**Iterators**: any type with `fn next(&mut self) -> Option<T>` works
in `for`. Sequence combinators (`map`/`filter`/`take`/`skip`/`step_by`)
are callable as methods/free functions and materialize results.

## 10. Data structures

- `[T]` growable (push/pop/swap/sort/sort_by, `contains`, `index_of`,
  `first`/`last`, `rev`, `slice(a, b) -> Result`); `[T; N]`
  fixed; tuples `.0`/`.1`; tuple structs fully usable. Method-call
  `xs.insert/remove` are silent in-place; the Result-returning forms
  are the qualified `Vec::insert(xs, i, v)` / `Vec::remove(xs, i)`.
- `std::collections`: `Vec`, `HashMap` (struct/tuple keys by value;
  `iter()` yields `[(K, V)]`, `keys`, `values`, `HashMap::pop`),
  `HashSet` (full set algebra), `BTreeMap` (sorted; `String` or `i64`
  keys), `VecDeque`. A separate i64-only `queue`/`stack`/`deque`/
  `heap`/`ordered_*` family is functional re-bind style
  (`let q = queue::push(q, v)`), not mutating.
- Collection literals coerce to `Vec<T>`/`[T]` wherever the expected
  type calls for one.
- **Weak references**: RC means a genuine cycle leaks unless one edge
  is non-owning: `strong.downgrade() -> Weak<T>`,
  `w.upgrade() -> Option<T>`; `runtime::collect_cycles()` runs the
  collector on demand. TRAP: a `Weak` into a member of a genuinely
  strong cycle reads `Some` under `gos run` but `None` under `gos
  build` - break real cycles, cross-check with `gos build`.

## 11. Testing

Unit tests live in the file they cover under `#[cfg(test)] mod
<file>_tests { ... }` reaching items via `super::`. **Give each
file's test module a unique name** (`util_tests`, `parser_tests`):
multiple `mod tests` across bundled siblings collide (GR0003).
`assert(cond[, msg])` / `assert_eq(a, b[, msg])` are prelude
builtins; `std::testing::check*` record without panicking. `gos
test` also compiles and runs fenced code in doc comments.

```gossamer
#[cfg(test)]
mod arith_tests {
    use std::testing
    #[test]
    fn add_adds() { testing::check_eq(&super::add(2, 3), &5, "2+3") }
}
```

## 12. Stdlib map

Full path spelling is validated (GR0005); discover signatures with
`hover` / `gos doc` rather than memory. Modules by area:

- Core: `fmt io env fs path os process time context flag errors
  bytes bufio`. `process` is free functions - `process::run(prog,
  args) -> Result<{stdout, stderr, code}, String>` (real exec, no
  shell; NO `Command` builder - that is Rust-binding-only surface).
  Interactive children: `process::spawn_piped(prog, args) ->
  Result<Child, errors::Error>`; the `Child` drives piped stdio via
  `write_stdin(s)`, `close_stdin()`, `read_line() -> Option<String>`
  (the canonical drain is `while let Some(line) = child.read_line()`),
  `read_stdout()`, `wait() -> Result<i64, _>`, `kill()` - the
  JSON-RPC-over-stdio (MCP client) shape.
  `time`: `now_ms`, `Instant`, `Duration`, `sleep`, `after`,
  RFC 3339 parse/format. `flag`: `Set::new` + `string/int/bool`
  cells that auto-deref; no built-in required-flag.
- Text: `strings strconv utf8 unicode regex`. Prefer `to_lowercase` /
  `to_uppercase`; `split_once`, `substring(a, b)` clamps, `byte_at(i)`
  zero-fills OOB; Unicode 16 + UAX #31 identifiers (`let café = 1`);
  dynamic regex captures are positional in Gossamer code.
- Collections: section 10. Prelude scalar `min`/`max`/`clamp`
  (vec-shaped `min(xs)`/`max(xs)` return `Option<T>`).
- Encoding: `encoding::{json, yaml, toml, xml, csv, base64, base32,
  ascii85, hex, pem, binary}`. Typed serde is free functions with a
  turbofish - `from_json::<T>(&text)?` / `to_json::<T>(v)` (same for
  yaml/toml). Struct fields may be scalars, `String`, `Option<T>`,
  tuples, `HashMap<String, V>`, nested structs, and `Vec<T>` of those;
  fixed arrays/slices are rejected with a type diagnostic. For dynamic or
  partially-known shapes use `json::parse` + `get`/`at`/`as_i64`/
  `as_str`/`keys`/`len`. Unknown JSON keys are ignored.
- Web: `net` (Tcp/Udp/Unix sockets, `url`, `netip`, `ip`), `http`
  (client: `Client::builder()` or free `http::get/post/..`,
  `stream` for SSE; server: `http::serve(addr, handler)`,
  `Response::text/json`, `raw_body`, bodies cap 1 MiB),
  `http::router` (verb methods chain with `|>`; params via
  `r.path_value("name")` / `r.path_int("id")`), `http::websocket`
  (no `wss://` yet), `http_h3`, `http::static_files`, `http::proxy`,
  middleware/session/csrf/form/multipart/state/health, `html`,
  `mime`. Experimental: `html::template::render_json`, `tls`,
  `database::sql` (drivers via `[rust-bindings]`).
- Misc: `sort`, `math::{rand, big}`, `crypto::{rand, sha256, sha512,
  hmac, blake3, aead, ed25519, ecdsa, x509, kdf, password
  (Argon2id), subtle}` (`crypto::insecure` = MD5/SHA1 compat only),
  `hash::{fnv, crc32, adler32}`, `uuid` (v4/v7), `jwt`,
  `compress::{gzip, flate, zlib, zstd, bzip2}`, `archive::{zip,
  tar}`, `metrics`, `trace`, `slog`, `lifecycle`, `validate`,
  `testing`, `runtime` (`collect_cycles`, `set_panic_hook`).

```gossamer
use std::http
use std::http::router

let r = router::Router::new()
    |> _.get("/", handler_fn)
    |> _.get_fn("/ping", |_r| Ok(http::Response::text(200, "ok")))
http::serve("0.0.0.0:8080", r)?
```

## 13. Project layout

```
project.toml       # [project] id/version; [dependencies]; optional
                   # entry = "src/app.gos"; [rust-bindings] for
                   # native Rust crates (gos bindgen skeletons)
src/main.gos       # binary entry (lib.gos for libraries;
                   # subdir/mod.gos for module `subdir`)
tests/             # integration tests
```

A multi-package checkout uses `[workspace] members = ["packages/*"]`
at the root (`gos new ID --template workspace`). Only the resolved
entry file may carry top-level statements.

## 14. Sharp edges and tier notes

The surface runs bit-identically across the VM (`gos run`/`gos
test`), the Cranelift JIT, and LLVM AOT (`gos build`); a divergence
you can reduce is a bug - check against both `gos run` and `gos
build`. Known sharp edges:

- `+` on `String` copies; heavy assembly wants `bytes::Builder` or a
  `mut String` with `+=`.
- Method dispatch is name-global in places: qualified paths
  (`Point::origin()`) always work; `String`/`HashMap`/`Vec` receivers
  dispatch by type; don't shadow built-in method names.
- `u64`/`usize` at or above 2^63: the VM and LLVM tiers compare/
  shift/display by declared type, but the in-process JIT still
  treats them as signed - cross-check hot large-`u64` code with
  `gos build`.
- Per-file test modules need unique names (GR0003; section 11).
- `Weak` into a strong cycle diverges across tiers (section 10).
- Not implemented (parse or reject cleanly): `async`/`await`,
  explicit lifetimes, the `move` keyword (capture is automatic).
  `gos feature-status` lists Experimental/Planned surface.

## 15. The `gos` toolchain

Bare `gos` opens the REPL. In a project, `gos run` / `gos build`
resolve the entry themselves.

| Command | Purpose |
|---------|---------|
| `gos check / parse / run / build FILE` | Typecheck; AST dump; VM+JIT run; fast native build. |
| `gos build --release [--target T]` | Full LLVM `-O3` (static-musl on Linux); cross to `{x86_64,aarch64}-unknown-linux-{gnu,musl}`. |
| `gos test / bench PATH` | `#[test]` / `#[bench]`; `--coverage`, `--parallel N`, `--format junit`, `--tier-parity`. |
| `gos fmt [--check] / lint / doc / explain CODE` | Format; lints; item docs; diagnostic rationale. |
| `gos mcp / lsp / repl` | MCP server for agents; stdio LSP for editors; REPL. |
| `gos new / init / add / remove / tidy / vendor / publish` | Scaffold and package management (Ed25519-signed registry). |
| `gos watch / clean / env / completion / bindgen / feature-status` | Re-run on change; caches; toolchain info; shell completions; Rust-binding skeletons; feature registry. |
| `gos skill-prompt` | Print this card (`gos skill-prompt \| claude --append-system-prompt`). |

## 16. When in doubt

Run it (section 2). Spec: `SPEC.md`. Style: `GUIDELINES.md`.
Examples: `examples/` - start with `hello_world.gos`,
`function_piping.gos`, `concurrency.gos`.
