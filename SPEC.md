# Gossamer Language Specification

> Status: pre-1.0.0 draft. Models the current Gossamer language —
> a language targeting the 2026+ Rust ecosystem. The CLI toolchain is
> `gos` (single unified binary in the spirit of `go` or `cargo`).
> Source files use the extension `.gos`. The manifest file is
> `project.toml`; the lockfile is `project.lock`.

---

## 1. Introduction

Gossamer is a general-purpose, garbage-collected, statically typed
programming language with first-class concurrency. It runs on the same
target set as Go, compiles to a single self-contained binary, and shares
Go's runtime model (M:N goroutine scheduler, channels, concurrent
garbage collection). Its surface syntax, type system, and error-handling
discipline are taken from Rust (2024 edition): `fn` declarations, `let`
bindings, `struct`/`enum`/`trait`/`impl`, pattern matching, `Option` and
`Result`, the `?` operator, monomorphized generics.

Gossamer deliberately omits:

- Lifetimes and the borrow checker (GC removes the need).
- Manual memory management and `Drop` semantics tied to stack frames.
- `nil`/`null` of any kind.
- Raw pointers in safe code.
- Exceptions.

The language is designed so that:

- **Character economy.** Fewer keystrokes per idea. Gossamer is an
  AI-friendly language: most programs in this codebase are written,
  read, and edited by AI agents, and token count in model context is a
  real cost. Given two equally-clear forms, the shorter one wins.
  Concrete consequences:
  - One line-comment syntax (`//`), not three (`//`, `///`, `//!`).
  - One block-comment syntax (`/* */`), not three.
  - Short keywords where unambiguous (`fn`, not `function`; `use`, not
    `import`; `mut`, not `mutable`).
  - Punctuation over keywords when equally clear (`|>` over `pipe`).
  - No ceremonial scaffolding: no empty `init()` functions, no class
    boilerplate, no annotation blocks where a sigil suffices.
  This principle resolves style disputes: when two forms are otherwise
  equivalent, pick the one with fewer characters.
- A single pass over the source file classifies it into tokens.
- A single recursive-descent parser produces an AST without
  context-dependent parsing tricks beyond bounded lookahead.
- The type system is decidable and cheap to check (no higher-rank types,
  no higher-kinded types, no type-level computation beyond const
  evaluation of sizeof-like queries).
- Compile speed is competitive with Go (target: 50k-100k LoC/s
  single-core frontend throughput; Cranelift backend for debug builds
  adds <30% over frontend time).

Notation follows the Go specification's EBNF conventions. Lowercase
productions are lexical terminals; CamelCase productions are grammatical
non-terminals.

---

## 2. Source representation

Source files are UTF-8 encoded Unicode. Files have extension `.gos`
(placeholder). One file declares exactly one package (see §15).

```
newline        = U+000A
unicode_char   = any Unicode code point except newline
letter         = unicode_letter | "_"
decimal_digit  = "0" ... "9"
```

Whitespace is any sequence of U+0020, U+0009, U+000D, U+000A. Unlike Go,
Gossamer **does not perform automatic semicolon insertion**. Statements
are terminated by the end of the expression they produce, or by a
newline after expressions that don't continue on the next line. Blocks
(`{ ... }`) serve as the primary delimiters.

### 2.1 Comments

- Line: `// ... <newline>`
- Block: `/* ... */` (may not nest).

There is no separate doc-comment syntax. A run of `//` comments
immediately preceding an item (no blank line between) is its
documentation; a run of `//` comments at the very top of a file is the
module's documentation. Tooling reads these by position. This keeps one
comment form instead of three (`//`, `///`, `//!`).

### 2.2 Tokens

Four classes: identifiers, keywords, literals, punctuation. The longest
legal match rule applies.

### 2.3 Identifiers

```
identifier = letter { letter | unicode_digit } .
```

Identifiers are case-sensitive. `_` alone is the "discard" pattern and
is not a binding.

Visibility follows Rust: items are private by default. Public items use
the `pub` keyword. Gossamer does not use Go's capitalization-based
visibility rule.

### 2.4 Keywords

Reserved:

```
as        async     await     break     const     continue
crate     defer     else      enum      extern    false
fn        for       go        if        impl      in
let       loop      match     mod       mut       pub
return    select    self      Self      static    struct
super     trait     true      type      unsafe    use
where     while     yield
```

Reserved but currently unused (future extensions): `async`, `await`,
`crate`, `yield`, `extern`, `package`.

`use` is the sole path-binding keyword; there is no `import`. `package`
is reserved but has no role — source files do not declare a package;
see §6. `move` is **not** a keyword: Gossamer has no ownership
transfer, so the Rust-style `move` closure qualifier would be
meaningless. Closures capture by GC reference for heap types and by
copy for `Copy` types with no opt-in needed.

### 2.5 Operators and punctuation

```
+  -  *  /  %
&  |  ^  <<  >>
+= -= *= /= %= &= |= ^= <<= >>=
=  ==  !=  <  <=  >  >=
!  &&  ||
|>                                  // pipe (F#-style forward pipe)
.  ..  ..=  ...  ::  ->  =>
(  )  [  ]  {  }
,  ;  :  ?  #  @
```

Unlike Rust, Gossamer does not use `&` to mean "borrow" — `&expr` takes
a GC-managed reference (see §4.3). The `*` operator is used only for
pointer dereference inside `unsafe` blocks; regular method/field access
auto-dereferences.

### 2.6 Literals

Integer, float, string, char (rune), bool, unit (`()`).

```
int_lit     = decimal_lit | bin_lit | oct_lit | hex_lit
decimal_lit = digit { digit | "_" }
bin_lit     = "0b" bin_digit { bin_digit | "_" }
oct_lit     = "0o" oct_digit { oct_digit | "_" }
hex_lit     = "0x" hex_digit { hex_digit | "_" }

float_lit   = decimal_digits "." decimal_digits [ exponent ]
            | decimal_digits exponent
            | "." decimal_digits [ exponent ]
exponent    = ( "e" | "E" ) [ "+" | "-" ] decimal_digits

char_lit    = "'" ( unicode_char | byte_escape | unicode_escape ) "'"

string_lit  = "\"" { string_char | escape } "\""
raw_string  = "r\"" { raw_char } "\"" | "r#\"" { raw_char } "\"#"

byte_lit    = "b'" byte_char "'"
byte_string = "b\"" { byte_char } "\""
```

Literal suffixes disambiguate type:

- `42i32`, `42u64`, `42usize` — typed integer literals.
- `3.14f32`, `3.14f64` — typed float literals.
- Untyped literals default to `i64` / `f64` unless context infers otherwise.

Integer literals may contain `_` as a visual separator anywhere after
the first digit and not adjacent to the decimal point or exponent
marker.

### 2.7 Statement termination

A block is a sequence of statements and an optional trailing expression.
Statements are either ended by `;` or are self-terminated by their
trailing `}` (for control-flow constructs). An expression without a
trailing `;` at the end of a block is the block's value. This mirrors
Rust 2024 exactly.

Example:

```
fn abs(n: i32) -> i32 {
  if n < 0 { -n } else { n }          // trailing expression, no ';'
}
```

Unlike Go, Gossamer never auto-inserts `;`. The lexer emits tokens
verbatim; the parser consumes whitespace/newlines only as separators.

---

## 3. Types

### 3.1 Built-in primitive types

- Signed ints: `i8`, `i16`, `i32`, `i64`, `i128`, `isize`.
- Unsigned ints: `u8`, `u16`, `u32`, `u64`, `u128`, `usize`.
- Floats: `f32`, `f64`.
- `bool` (1 byte).
- `char` — a 32-bit Unicode scalar value (not a surrogate).
- `()` — the unit type, inhabited by the value `()`.
- `!` — the never type (uninhabited; result type of `panic!`, `return`,
  infinite loops).

**Integer overflow semantics.** The `+`, `-`, `*`, `<<`, `>>` operators
on integer types:

- Panic on overflow in debug builds (`gos build` without `--release`).
- Wrap modulo 2<sup>N</sup> in release builds (`gos build --release`).

Silent wrap is never the default in any build mode — the release
behaviour is explicit two's-complement wrap, not "undefined." The
method forms `checked_add`, `wrapping_add`, `saturating_add`,
`overflowing_add` and friends are always available for explicit
disambiguation and are the required form in code that must behave
identically across build modes.

**No implicit numeric widening.** All numeric conversions — widening or
narrowing — require an explicit `as` cast. `let bigger: i64 = small_i32`
is a type error; write `let bigger = small_i32 as i64`. This prevents
silent truncation, silent sign changes, and surprise precision loss.

### 3.2 Strings

`String` is an immutable, growable (via `+`, `push_str`, etc.) GC-backed
UTF-8 string. Because `String` is GC-managed, there is no `&str`/`String`
distinction and no lifetime parameter. String literals have type
`String`.

`char` is a 32-bit Unicode scalar value. A `String` is not indexable by
`char`; iteration is via `.chars()` (an iterator of `char`) or
`.bytes()` (an iterator of `u8`). Byte-level substring operations go
through `.as_bytes()` which returns `&Vec<u8>` sharing the underlying
bytes.

### 3.3 Collections (built-in generic types)

| Type | Semantics |
|---|---|
| `Vec<T>` | Growable slice. Analogue of Go's `[]T`. |
| `Array<T, N>` | Fixed-size array. Analogue of Go's `[N]T`. |
| `HashMap<K, V>` | Hash map. Analogue of Go's `map[K]V`. |
| `BTreeMap<K, V>` | Ordered map. |
| `HashSet<T>`, `BTreeSet<T>` | Sets. |
| `Sender<T>`, `Receiver<T>` | Channel endpoints. Always come as a pair from `channel<T>()`. |

All collections are GC-managed reference types. Assigning
`let b = a;` where `a: Vec<T>` creates a second reference to the same
underlying slice (same as Go's behavior). For a deep copy, use
`.clone()`.

### 3.4 Pointers and references

- `T` — a value. For `Copy` types (primitive numerics, `bool`, `char`,
  small POD structs) pass/return is a copy. For heap-managed types,
  `T` still names a GC reference — assignment and parameter passing
  create a second reference to the same heap cell. There is no
  ownership transfer and no `move` keyword: the original binding
  remains accessible.
- `&T` — a **shared GC reference** to a value of type `T`. Not a raw
  pointer. Cannot be null. Created by `&expr`. Auto-dereferenced for `.`
  access. Liveness is guaranteed by the GC; the compiler additionally
  enforces a local aliasing discipline described in §7.5.
- `&mut T` — an **exclusive GC reference**. Required to mutate through a
  reference. Cannot coexist with any other reference (shared or
  exclusive) to the same value within a function body. Cannot appear as
  a struct field. Cannot cross a `go` or channel boundary.
- `*const T`, `*mut T` — raw pointers. Only constructible and usable
  inside `unsafe` blocks. Used for FFI.

`&T` and `&mut T` in Gossamer are not borrows in the Rust sense — GC
already guarantees liveness. They are access-mode markers used by a
scope-local check (§7.5) to prevent simultaneous mutation and reading
of the same value. No lifetime parameters exist at any level of the
language.

### 3.5 Function types

```
FnType = "fn" "(" [ TypeList ] ")" [ "->" Type ]
       | "Fn" "(" [ TypeList ] ")" [ "->" Type ]       // closure trait
       | "FnMut" "(" [ TypeList ] ")" [ "->" Type ]
       | "FnOnce" "(" [ TypeList ] ")" [ "->" Type ]
```

Plain `fn(...) -> ...` is a non-capturing function pointer. `Fn`,
`FnMut`, `FnOnce` are closure traits (as in Rust). Closures that capture
the environment satisfy the appropriate closure trait and are
GC-allocated. Because there is no borrow checker, `Fn` and `FnMut`
collapse into essentially the same constraint; the distinction is
retained for readability and forward compatibility.

### 3.6 Structs

```
StructDecl = [ "pub" ] "struct" Ident [ Generics ] StructBody [ WhereClause ]
StructBody = "{" [ FieldList ] "}"
           | "(" [ TypeList ] ")"        // tuple struct
           | ";"                          // unit struct
FieldList  = Field { "," Field } [ "," ]
Field      = [ "pub" ] Ident ":" Type
```

Struct values are allocated inline when they are local variables, but
may escape to the GC heap via escape analysis (any field mutation
through a `&T`, any storage in a channel, any capture by a closure that
outlives the caller, etc.).

Example:

```
pub struct Point { pub x: f64, pub y: f64 }
struct Wrapper(i32, i32);     // tuple struct
struct Marker;                // unit struct
```

### 3.7 Enums (sum types)

```
EnumDecl = [ "pub" ] "enum" Ident [ Generics ] "{" VariantList "}" [ WhereClause ]
Variant  = Ident [ "(" TypeList ")" | "{" FieldList "}" ]
```

Enum values carry a discriminant and the payload of the active variant.
The built-in `Option` and `Result` are defined as:

```
pub enum Option<T> { Some(T), None }
pub enum Result<T, E> { Ok(T), Err(E) }
```

### 3.8 Traits

```
TraitDecl = [ "pub" ] "trait" Ident [ Generics ] [ ":" BoundList ] "{" TraitItems "}"
TraitItem = FnSig ";"
          | FnDecl                         // with default body
          | "type" Ident [ ":" BoundList ] [ "=" Type ] ";"
          | "const" Ident ":" Type [ "=" Expr ] ";"
```

Traits support:

- Required and default methods.
- Associated types and associated constants.
- Bounds on trait generics.
- Supertraits (`trait Foo: Bar + Baz`).
- Default methods.

Gossamer does **not** support:

- Higher-ranked trait bounds (`for<'a> ...`).
- Object-safety nuances beyond a simple rule: a trait is dyn-compatible
  iff it has no associated types without defaults, no `Self:Sized`
  constraints in methods, and no generic methods.

### 3.9 Impl blocks

```
ImplDecl      = "impl" [ Generics ] Type [ WhereClause ] "{" ImplItems "}"
ImplDeclTrait = "impl" [ Generics ] TraitRef "for" Type [ WhereClause ] "{" ImplItems "}"
```

Inherent impls attach methods/associated items to a type. Trait impls
declare that a type satisfies a trait.

Method receivers:

- `fn m(self)` — receives the value by copy (Copy types) or by GC
  reference (heap types). The caller's binding remains usable after
  the call.
- `fn m(&self)` — shared access. Under GC this is just "pass the ref".
- `fn m(&mut self)` — exclusive access. Same runtime as `&self`; used
  by the type checker and the local borrow check (§7.5) to forbid
  method calls on non-mut bindings and to prevent simultaneous aliases
  within a function body.

### 3.10 Generics

```
Generics  = "<" GenericParam { "," GenericParam } ">"
GenericParam = LifetimeParam | TypeParam | ConstParam
TypeParam = Ident [ ":" BoundList ] [ "=" Type ]
ConstParam = "const" Ident ":" Type [ "=" Expr ]
```

Lifetime parameters exist syntactically only for FFI signatures that
mirror Rust crates — they are parsed and ignored by the type checker in
safe code. In normal code, lifetimes are never written.

Generic instantiation in expressions uses the turbofish `::<T>`:

```
let v = Vec::<i32>::new()
let (tx, rx) = channel::<String>()
```

The bare form `name<T>(...)` is also accepted when the parser can
disambiguate with one-token lookahead after the closing `>` (must be
`(`, `::`, or `{`).

### 3.11 Dynamic dispatch

`dyn Trait` is a trait object (fat pointer: data + vtable). Allocated
on the GC heap.

```
let handlers: Vec<dyn Handler> = vec![...]
```

Unlike Rust, no explicit `Box<dyn Trait>` is needed — because values in
heap-allocated collections are already GC-managed references, writing
`dyn Handler` as an element type is sufficient.

### 3.12 Type aliases

```
TypeAlias = "type" Ident [ Generics ] "=" Type ";"
```

---

## 4. Variables, expressions, statements

### 4.1 Bindings

```
LetStmt = "let" [ "mut" ] Pattern [ ":" Type ] [ "=" Expr ] ";"
```

- `let x = 1` — immutable binding, type inferred.
- `let mut x = 1` — mutable binding.
- `let (a, b) = pair` — destructuring.
- `let Point { x, y } = p` — struct destructuring.
- `let x: i64 = 1` — annotated.

Shadowing is permitted.

### 4.2 Expressions

Every construct except `let`, `use`, item declarations, and control
flow **statements with a trailing `;`** is an expression. Block
expressions return their tail expression:

```
let n = {
  let x = 2
  x * x
}
```

The control-flow constructs `if`, `match`, `loop`, `while`, `for`,
`unsafe { ... }`, and `{ ... }` are expressions. `while` and `for`
evaluate to `()`. `loop` can return a value via `break value;`.

### 4.3 Reference expressions

`&expr` creates a GC reference. `&mut expr` is the same runtime but
requires `expr` to be a mutable place. In the absence of lifetimes and
borrow checking, this is pure ergonomics.

`*expr` (inside `unsafe`) dereferences a raw pointer. Regular
`&T -> T` dereference is implicit at `.` and index operators.

### 4.4 Control flow

#### `if`

```
IfExpr = "if" Expr Block [ "else" ( IfExpr | Block ) ]
```

An `if` without an `else` has type `()`. With `else`, both arms must
produce the same type (or one is `!`).

#### `match`

```
MatchExpr = "match" Expr "{" MatchArm { "," MatchArm } [ "," ] "}"
MatchArm  = Pattern [ "if" Expr ] "=>" ( Expr | Block )
```

`match` is exhaustive. Non-exhaustive `match` is a compile error.
Patterns support literals, wildcards (`_`), ranges (`1..=10`),
bindings, struct/enum destructuring, and or-patterns (`A | B`).

```
match divide(a, b) {
  Ok(v) => fmt::println("got:", v),
  Err(e) => fmt::eprintln("err:", e),
}
```

#### `while`, `loop`, `for`

```
WhileExpr  = "while" Expr Block
LoopExpr   = "loop" Block
ForExpr    = "for" Pattern "in" Expr Block
```

`for` desugars to a loop that calls `.next()` on an iterator. Any type
implementing `Iterator<Item = T>` (see §10.4 on stdlib traits) can be
ranged over. The built-in ranges `a..b` and `a..=b` implement
`Iterator`.

#### `break`, `continue`

`break [expr]` exits the innermost loop (value only valid in `loop`).
`continue` jumps to the next iteration. Labeled variants
(`'outer: loop { break 'outer; }`) are supported.

#### `return`

`return expr;` exits the enclosing function. `return;` returns `()`.

#### `defer`

```
DeferStmt = "defer" Block
```

Like Go's `defer`, but takes a block instead of a single call. Deferred
blocks run in LIFO order when the enclosing function returns (normally
or via panic).

```
fn read_all(path: String) -> Result<Vec<u8>, Error> {
  let file = os::open(path)?
  defer { file.close() }
  file.read_to_end()
}
```

Captured variables in a `defer` block are snapshotted at the time of
the `defer`, following the same semantics as Go.

#### `go`

```
GoStmt = "go" Expr
```

The expression must be a call (possibly the call of an anonymous `fn()`
literal, in which case the `()` on the literal may be omitted as
syntactic sugar). Launches the call in a new goroutine; does not wait.

```
go worker()
go producer.step()
go fn() { process(item) }          // sugar for: go (fn() { process(item) })()
```

#### `select`

```
SelectExpr = "select" "{" SelectArm { "," SelectArm } [ "," ] "}"
SelectArm  = RecvPattern "=" RecvExpr "=>" ( Expr | Block )
           | SendExpr            "=>" ( Expr | Block )
           | "default"           "=>" ( Expr | Block )
RecvExpr   = Expr ".recv()"
SendExpr   = Expr ".send(" Expr ")"
```

`select` chooses exactly one of its communication operations to proceed,
pseudo-randomly among those ready. If none is ready and no `default`
arm exists, the goroutine blocks. Matches Go's select semantics.

Example (from examples.md):

```
select {
  Ok(msg) = rx_ok.recv() => fmt::println("success:", msg),
  Err(err) = rx_err.recv() => fmt::println("error:", err),
}
```

The binding pattern on the left (`Ok(msg)`) matches the `Result` returned
by `.recv()` (see §8.3).

### 4.5 The `?` operator

```
TryExpr = Expr "?"
```

If applied to `Result<T, E>`, it evaluates to `T` on `Ok`, or returns
`Err(From::from(e))` from the enclosing function on `Err`. If applied
to `Option<T>`, evaluates to `T` on `Some`, or returns `None` on `None`.
The enclosing function's return type must be `Result<_, E2>` (where
`E: Into<E2>`) or `Option<_>` respectively.

### 4.6 Pipe expression (F#-style forward pipe)

```
PipeExpr = Expr "|>" Expr
```

The forward-pipe operator `|>` feeds the value of its left operand to
the callable on its right. Semantics follow F#: the piped value is
passed as the **last** positional argument of the right-hand call.
The operator is **left-associative** and has very low precedence (just
above assignment), so `a |> f |> g` parses as `(a |> f) |> g` and means
`g(f(a))`.

Desugaring rules (applied after parsing, before HIR lowering):

1. `x |> path` where `path` resolves to a callable of arity 1:
   → `path(x)`.
2. `x |> path(a1, ..., an)` where `path` is callable of arity `n+1`:
   → `path(a1, ..., an, x)`.
3. `x |> recv.method` (no parens):
   → `recv.method(x)`.
4. `x |> recv.method(a1, ..., an)`:
   → `recv.method(a1, ..., an, x)`.
5. `x |> (closure_expr)` where `closure_expr` evaluates to a callable:
   → `(closure_expr)(x)` (arity must be 1).
6. `x |> path::<T1, ..., Tk>(a1, ..., an)`:
   → `path::<T1, ..., Tk>(a1, ..., an, x)`.

If the right operand is not a call form matching one of the above, the
compiler emits `E0601: right-hand side of '|>' must be a callable`.

Type-checking rule: the type of the piped value must unify with the
type of the implicit trailing parameter of the right-hand callable.
Method lookup, trait resolution, auto-deref, and the `?` operator all
apply to the desugared call exactly as they would to a hand-written
call.

Examples:

```
// Equivalent to: fmt::println(format!("hello {}", name))
name |> format!("hello {}", ..) |> fmt::println
```

Note: the `..` placeholder is **not** required — `|>` implicitly
targets the last position. The code above would equivalently be
written:

```
name |> format!("hello {}", _) |> fmt::println   // _ is optional, sugar
```

The explicit placeholder forms (`..` or `_`) are purely documentary;
the parser accepts them at the trailing position for readability but
strips them during desugaring.

Idiomatic iterator chains:

```
let total =
  1..=100
  |> iter::filter(|n| n % 2 == 0)
  |> iter::map(|n| n * n)
  |> iter::sum::<i64>()
```

Desugars to:

```
let total = iter::sum::<i64>(iter::map(|n| n * n, iter::filter(|n| n % 2 == 0, 1..=100)))
```

Interaction with `?`:

```
read_file(path)? |> parse_json::<Config>()?
```

Here `?` binds tighter than `|>` (§4.7 precedence), so this parses as
`(read_file(path)?) |> (parse_json::<Config>()?)` — the inner `?`
unwraps the `Result<String, _>`, pipes the `String` into
`parse_json`, and the outer `?` unwraps that result.

### 4.7 Operators and precedence

From highest to lowest:

| Level | Operators | Associativity |
|---|---|---|
| 1 | `::` path | left |
| 2 | `.` method/field, `[]`, `()`, `?`, postfix | left |
| 3 | unary `-`, `!`, `&`, `&mut`, `*` (unsafe) | right |
| 4 | `as` cast | left |
| 5 | `*`, `/`, `%` | left |
| 6 | `+`, `-` | left |
| 7 | `<<`, `>>` | left |
| 8 | `&` bitand | left |
| 9 | `^` bitxor | left |
| 10 | `\|` bitor | left |
| 11 | `==` `!=` `<` `<=` `>` `>=` | none (non-associative) |
| 12 | `&&` | left |
| 13 | `\|\|` | left |
| 14 | `..` `..=` range | none |
| 15 | `\|>` pipe | left |
| 16 | `=`, `+=`, `-=`, etc. (statement-only) | right |

---

## 5. Patterns

```
Pattern = LiteralPattern
        | IdentPattern                      // binding
        | "_"                                // wildcard
        | "(" Pattern { "," Pattern } ")"   // tuple
        | Path ( "(" PatternList ")" | "{" FieldPatternList "}" )  // struct/enum
        | Pattern "|" Pattern                // or-pattern
        | Literal ".." Literal               // range
        | Literal "..=" Literal              // range inclusive
        | "&" Pattern                        // ref pattern
        | "mut" IdentPattern                 // mutable binding
        | ".." Pattern?                      // rest pattern
```

Exhaustiveness is checked via matrix decomposition (the Maranget
algorithm, same as Rust).

---

## 6. Projects, modules, and source files

Gossamer cleanly separates two concepts that other languages often
conflate:

- **Module** — how code is *organized* into namespaces. No version, no
  owner, no network identity.
- **Project** — how code is *distributed* and *versioned*. Carries a
  stable domain-based identifier, a semver, and dependency
  declarations.

A project contains one module or many. A module never spans projects.

### 6.1 Source files

A source file is plain Gossamer; it does not declare a package, does
not declare its module, and contains no boilerplate header.

```
SourceFile = { UseDecl } { Item }
```

A file's module is determined by its location on disk (§6.3). Its
owning project is determined by the nearest enclosing `project.toml`
walked upward from the file (§6.4).

### 6.2 Paths

Two path separators, each with one meaning:

- `::` separates **module/name** components:
  `math::vector::Vec3`.
- `.` accesses a **field or method** on a value: `v.x`, `s.len()`.

There is no third separator. Project identifiers, despite containing
`.` and `/` characters, are always written as string literals in `use`
declarations (§6.6).

### 6.3 Modules (code organization)

Modules are directory-based by default. Given a project layout:

```
my-project/
  project.toml
  src/
    main.gos
    math.gos
    math/
      vector.gos
      matrix.gos
    net/
      http.gos
      tcp.gos
```

- Every `.gos` file directly in `src/` contributes items to the
  project's root module.
- Each subdirectory of `src/` is a module named after the directory;
  every `.gos` file inside it contributes items to that module.
- Modules nest: `src/math/vector.gos` is `math::vector`.
- An optional `mod.gos` file inside a module directory holds
  module-level comments and re-exports.

Explicit inline modules are supported for cases where directory
splitting is overkill:

```
mod vector {
    struct Vec3 { x: f64, y: f64, z: f64 }
}
```

Items within the same module reference each other by bare name. Items
in a sibling or nested module use a path: `math::vector::Vec3`.

### 6.4 Projects (unit of distribution)

A **project** is defined by a `project.toml` manifest at its root. It
is the unit of distribution, versioning, and dependency declaration.

```toml
[project]
id      = "example.com/math"
version = "0.3.1"
authors = ["Jane Doe <jane@example.com>"]
license = "Apache-2.0"

[dependencies]
"example.org/linalg"   = "1.2"
"example.com/logging"  = { git = "https://git.example.com/logging.git", tag = "v0.8.0" }
"example.net/internal" = { path = "../internal" }

[registries]
"example.org" = "https://registry.example.org/v1"
```

Required fields:

- `project.id` — the project identifier (see §6.5).
- `project.version` — semver `MAJOR.MINOR.PATCH`.

Every other key is optional.

### 6.5 Project identifiers

A project identifier is a stable, location-independent string of the
form:

```
ProjectId = DomainSegment { "/" PathSegment }
DomainSegment = Label { "." Label }        // must contain at least one "."
Label         = [a-z][a-z0-9-]*
PathSegment   = [a-z0-9][a-z0-9-]*
```

Examples: `example.com/math`, `acme.dev/tools/codegen`,
`fooware.io/json`.

Properties:

- The identifier is **not** a URL. It names no server, no repository
  service, and no protocol. Resolution to a physical source is the
  toolchain's job.
- It is not tied to any hosting provider. `github.com/...` as an
  identifier is discouraged because it couples identity to a service;
  use a domain you control.
- Ownership is social, not technical. No global authority enforces who
  may publish under a prefix — disputes are resolved by consumers
  choosing which dependency to declare.
- Short identifiers (single-segment: `math`, `fmt`) are **reserved for
  the standard library**.

### 6.6 `use` declarations

```
UseDecl    = "use" UseTarget [ "as" Ident ] [ "{" UseList "}" ]
UseTarget  = ProjectUse | ModulePath
ProjectUse = StringLit [ "::" ModulePath ]
ModulePath = Ident { "::" Ident }
UseList    = Ident [ "as" Ident ] { "," Ident [ "as" Ident ] } [ "," ]
```

A `use` target is either a string-literal project reference or an
identifier-based module path within the current project.

```
// Bring another project into scope. Bound name defaults to the last
// segment of the project id.
use "example.com/math"                        // binds `math`
use "example.com/math" as m                   // binds `m`

// Reach into a specific module of another project.
use "example.com/math"::vector                // binds `vector`
use "example.com/math"::vector::{Vec3, Vec4}

// Same-project paths use ordinary module syntax — no string.
use vector::{Vec3, Vec4}
use net::http::Server

// Standard library uses a reserved single-segment identifier and needs
// no string literal.
use std::io
use std::sync::atomic::{AtomicU64, Ordering}
use fmt
```

The string-literal form is mandatory for any project whose identifier
contains `.` or `/`, which is every real-world external dependency.
Identifier-only paths never escape the current project.

There is no side-effect-only `use`. A project's initialisation is
explicit through an optional `fn init()` per module, run in
dependency-topological order at program start.

### 6.7 Dependency resolution (tool-driven)

Dependency resolution is the job of the `gos` tool (§16). The compiler
itself never fetches code; it reads a resolved source tree the tool
prepared. The tool resolves each entry in `[dependencies]` by source
kind:

- **Registry**: the dependency's project-id prefix is matched against
  the `[registries]` table. A registry is a plain HTTP endpoint
  exposing signed tarballs. No central registry exists or is required;
  `[registries]` may be empty. Multiple registries coexist without
  conflict because each serves distinct domain prefixes.
- **Git**: `{ git = "...", tag = "..." | branch = "..." | rev = "..." }`.
  The tool clones the repository, expects a `project.toml` at its
  root, and verifies that the manifest's `project.id` matches the
  dependency entry's key.
- **Local path**: `{ path = "../other" }`. For developing related
  projects side by side; forbidden in published manifests.
- **URL tarball**: `{ url = "https://...", sha256 = "..." }`. Plain
  fetch of an archive with a required checksum.

### 6.8 Reproducibility

On first resolution the tool writes `project.lock` recording, for
every transitive dependency:

- The resolved project identifier and version.
- The concrete source (git SHA, registry version, URL) it came from.
- A sha256 checksum of the source tree as fetched.

A checked-in lockfile yields byte-identical builds across machines.

### 6.9 Decentralisation

The design assumes and protects decentralised distribution:

- No single registry is required. Offline and air-gapped builds work
  via path dependencies and a `[replace]` table.
- Registries are optional and federated by DNS prefix.
- Direct git and URL dependencies remain first-class; a project can
  live forever without ever being published to a registry.
- Identifiers carry no global authority. If two projects claim the
  same identifier, consumers pick the right one by declaring the
  source explicitly.

### 6.10 Entry point

An executable project contains `src/main.gos` with a `fn main()`
returning either `()` or `Result<(), E>`. No `package main` clause is
required; presence of `src/main.gos` with a `fn main` is sufficient.

### 6.11 Rationale

Separating modules from projects matters because conflating them
(Go's "package == import unit == distribution unit") means every
rename, split, or move becomes a breaking change visible to every
caller. Domain-based identifiers matter because they give stable names
that survive hosting changes. A tool-driven resolver matters because
network fetching, checksums, and lockfiles are operational concerns
that do not belong in the language grammar. Decentralisation matters
because pinning a language to a single registry service hands control
of the ecosystem to whoever runs it.

---

## 7. Memory model

### 7.1 Allocation

All heap-allocated values are managed by the garbage collector. Values
that do not escape their defining function may be stack-allocated
(escape analysis). The escape rules are:

1. Any value whose address is taken (`&x`) and passed across a call
   boundary escapes.
2. Any value assigned to a field of a heap-allocated struct escapes.
3. Any value sent on a channel escapes.
4. Any value captured by a closure that is stored or passed beyond the
   creating scope escapes.

### 7.2 Garbage collection

The GC is a concurrent, tri-color, non-generational mark-sweep collector
with a Dijkstra-style insertion write barrier during the mark phase.
Collection is triggered by a heap-growth pacer identical in shape to
Go's: `GC_trigger = live_after_last_gc * (1 + GOGC/100)` with a default
`GOGC` of 100.

Stop-the-world is limited to:

- STW at start of mark (to install write barriers and scan roots).
- STW at end of mark (for termination marking).

Mutator work during mark includes:

- Write barrier recording of reference stores (shaded greying).
- Cooperative assist when the mutator is allocating faster than the
  concurrent collector can keep up.

Safe points are inserted by the compiler at:

- Every function prologue.
- Every loop back-edge.
- Every call site.

### 7.3 Zero values

Every type has a zero value:

- Numeric: `0`.
- `bool`: `false`.
- `char`: `'\0'`.
- `String`: empty string.
- `Vec<T>`, `HashMap<K, V>`, channel endpoints: `Empty`/`None`-like
  empty containers, not `None`.
- `Option<T>`: `None`.
- Enums: the first-declared variant, if it has no payload; otherwise
  the zero value for a type with no natural zero is a compile error if
  observable (types with no zero-default must be initialized).
- Structs: each field at its zero value.
- `fn` / closure types: **no zero value**. A field or variable of
  function type must be explicitly initialized. (Unlike Go's nil fn.)

### 7.4 Ordering and atomics

The memory model is the Go 1.19 memory model verbatim:

- Channel operations establish happens-before relationships.
- Mutex lock/unlock establish happens-before relationships.
- `sync::Once` establishes happens-before relationships.
- Atomics via `std::atomic` (sequentially consistent by default;
  relaxed/acquire/release available).

### 7.5 Local borrow checking

Gossamer has no ownership transfer, no `move` keyword, and no lifetime
annotations anywhere. All bindings stay live and accessible for the
duration of their lexical scope. Assignment, parameter passing, and
closure capture all behave like Go: a GC reference for heap-managed
types, a copy for `Copy` types. The cognitive load of "who owns this
value now" does not exist.

GC already guarantees that no reference dangles. Gossamer layers one
additional check on top: **within a single function body, a value may
have many shared `&T` references, or exactly one `&mut T` reference,
but never both at once.** The analysis is strictly scope-local — no
lifetime parameters, no cross-function inference, no whole-program
data-flow. One linear pass per function.

The check catches the bug class that GC cannot: iterator invalidation,
simultaneous mutation and iteration, and accidental self-aliasing.

#### 7.5.1 The rule

For every value `v` introduced in a function (parameter, `let` binding,
field projection rooted in either), at every program point the compiler
tracks the set of active references:

- Any number of active `&T` references to `v` are permitted
  simultaneously.
- Exactly one active `&mut T` reference to `v` is permitted, and only
  while no `&T` to `v` is active.
- Creating a reference that would violate the rule is a compile error.

A reference is *active* from the point of creation until the last
point it is observably used (non-lexical lifetimes, in the Rust sense).
After its last use, the reference is considered released and subsequent
references may be created.

```
let values = Vec::from([1, 2, 3])
let first = &values[0]          // shared ref active
values.push(4)                   // ERROR: push takes &mut self,
                                //        but first is still active
fmt::println(first)
```

```
let mut counter = 0
let a = &mut counter             // exclusive ref active
let b = &mut counter             // ERROR: cannot reborrow exclusively
*a += 1
```

#### 7.5.2 Function calls

A call `f(&mut x)` treats the argument as an exclusive borrow for the
duration of the call. When the call returns, the borrow is released in
the caller. Inside `f`, its body runs its own local check in exactly
the same way. No annotation flows across the boundary.

The compiler does not infer relationships between function return
values and their inputs. Returning a reference from a function is
permitted (GC keeps the pointee alive), but the caller receives an
unconstrained `&T` or `&mut T` that begins a fresh active range.

#### 7.5.3 Hard limits

Two patterns are outright forbidden to keep the analysis tractable:

- **Struct fields may not have type `&mut T`.** Tracking exclusivity
  through heap-stored references would require lifetime parameters.
  `&T` fields are fine — they are GC references, not tracked borrows.
- **No `&T` or `&mut T` crosses a `go` or channel boundary.** `go`
  and `Sender::send` pass values the same way ordinary assignment does
  — GC reference for heap types, copy for `Copy` types — and the
  caller retains access to its bindings. Tracked reference markers
  cannot cross because the local borrow check is scope-local. See
  §8.1 and §8.2. This rules out one source of cross-goroutine bugs
  but does not prevent data races on the underlying shared value;
  those remain the programmer's responsibility, caught at runtime by
  `--race` (post-v1) and prevented by channel-based communication.

#### 7.5.4 What this replaces

This design deliberately does *not* include:

- Explicit lifetime annotations (`'a`, `'static`, `for<'a>`).
- `Send`/`Sync` marker traits. The language accepts Go-style sharing
  discipline instead: communicate via channels; races on raw shared
  state are detected at runtime, not blocked at compile time.
- A region inference pass or SCC analysis. The check is one linear
  pass per function body.
- Any analysis that crosses function boundaries.

The goal is "catch most of what a borrow checker catches, for a
fraction of the implementation cost and zero annotation burden." The
escape hatch when the check rejects valid code is to restructure: take
an index instead of a reference, clone, or compute the value first and
then mutate.

---

## 8. Concurrency

### 8.1 Goroutines

A goroutine is a stackful coroutine scheduled cooperatively by the
runtime. `go expr` spawns one. Each goroutine owns a fixed-size
mmap'd stack (default 16 KiB; override with `GOSSAMER_GOROUTINE_STACK`).
The stack lives below a guard page, so overflow traps as a
deterministic SIGSEGV instead of clobbering arbitrary memory.

**Argument discipline.** `go` captures values by GC reference the same
way an ordinary closure does. `Copy` types (primitive numerics, `bool`,
`char`, small POD structs) are captured by value. After `go f(x)`
returns, the caller may continue to use `x` — Gossamer has no
ownership transfer, no `move` keyword, and no "value becomes invalid
after this point" semantics. This matches Go.

A `go` call may not capture or pass a `&T` or `&mut T`. The tracked
`&`/`&mut` access markers are scope-local (§7.5) and cannot be
reasoned about across goroutine boundaries; permitting them would
either require lifetime annotations (which we explicitly avoid) or
silently weaken the local check. Pass the underlying value (GC
reference, or `Copy`) instead.

Cross-goroutine data races on shared mutable state are possible — the
same trade-off Go makes. Detect them at runtime with `gos build
--race` (post-v1 tooling, tracked in the plan) and prevent them by
communicating through channels rather than sharing state.

The scheduler is an M:N work-stealing scheduler:

- **M** = OS thread (one per core by default, configurable via
  `GOSSAMER_MAX_PROCS` or `runtime::set_max_procs(n)`).
- **P** = processor (logical context, fixed count = max-procs).
- **G** = goroutine.

The network poller (epoll on Linux, kqueue on macOS/BSD, IOCP on
Windows) parks goroutines blocked on I/O without holding the
underlying OS thread. Same path covers `time::sleep` (timer wheel),
`channel.recv` / `channel.send` on a full or empty channel,
`sync::Mutex` contention, `sync::WaitGroup::wait`, and any
`std::fs` / `std::os::exec` syscall (which routes through a
shared blocking-syscall pool that parks the goroutine while a
real OS thread runs the syscall).

### 8.2 Channels

```
let (tx, rx) = channel::<T>()             // unbuffered
let (tx, rx) = channel::<T>(cap: 16)      // buffered
```

Channel operations (non-`select`):

- `tx.send(v)` — blocks until a receiver is ready (unbuffered) or
  buffer has capacity.
- `rx.recv() -> Result<T, ChannelClosed>` — blocks until a sender
  sends a value or the channel is closed (with drain).
- `rx.try_recv() -> Option<T>` — non-blocking.
- `tx.close()` — marks the channel closed. Subsequent sends panic.
  Receives drain buffered values then return `Err(ChannelClosed)`.

Channels are many-to-many. Close only once.

`Sender::send` passes a GC reference for heap-managed types and a
copy for `Copy` types — the same rules as ordinary assignment or
function call. No ownership transfer is implied; the sender retains
access to whatever bindings it named. Sending a `&T` or `&mut T` on a
channel is a compile error, for the same reason `go` cannot capture
references: the local borrow check (§7.5) is scope-local and cannot
follow an access marker across goroutine boundaries.

### 8.3 Select

Each arm of `select` is a communication operation. The `rx.recv()`
call returns `Result<T, ChannelClosed>`, so pattern-matching the arm
on `Ok(v)` or `Err(_)` is how closed channels surface:

```
select {
  Ok(v) = ch.recv() => process(v),
  Err(_) = ch.recv() => { break; }       // channel closed
  default              => do_other(),
}
```

Because the language does not have Go's "comma-ok" idiom, the `Result`
return type replaces it.

### 8.4 `defer` and goroutines

Deferred blocks run when the **goroutine** (not the program) unwinds
past the enclosing function. Panics within a goroutine unwind that
goroutine's stack, running its defers. A panic that is not recovered
inside the goroutine crashes the whole process (like Go).

### 8.5 `recover`

`std::panic::catch_unwind(|| { ... })` returns `Result<T, PanicPayload>`,
catching panics inside the closure. This replaces Go's `recover()`.

### 8.6 `unsafe`

```
unsafe { ... }
unsafe fn raw_thing() { ... }
```

`unsafe` blocks/functions permit:

- Raw pointer (`*const T`, `*mut T`) deref.
- Calling `extern "C"` functions.
- Calling other `unsafe fn`.

They do **not** disable GC or suspend safepoints.

---

## 9. Error handling

Errors are values of types implementing the `Error` trait:

```
pub trait Error: Display + Debug {
  fn source(&self) -> Option<&dyn Error> { None }
}
```

Use `Result<T, E>` to signal failure; `?` to propagate. `panic!` is for
unrecoverable conditions only (array out-of-bounds, unwrap on `None`,
divide by zero on integers, explicit `panic!` in code).

No exceptions, no `throw`, no `try/catch` in user code (the `?`
operator handles control flow).

**`Result` is `#[must_use]` by default.** A `Result<T, E>` expression
used as a statement (its value discarded) is a compile error unless
the type is explicitly ignored with `let _ = expr` or the function is
annotated `#[allow(unused_result)]`. Dropping an error on the floor
must be an intentional act. The same treatment applies to `Option<T>`
only when the function producing it is itself marked `#[must_use]`.

---

## 10. Standard library

This is an outline; full API docs ship with the first implementation.

### 10.1 `std::fmt`

- `println(args...)` — variadic print-with-newline. Each argument
  implements `Display`.
- `eprintln(args...)`.
- `format!(fmt_str, args...)` — returns `String`. `fmt_str` is a
  compile-time-validated format string (`{}` placeholders).
- `print`, `eprint` without newline.
- `Display`, `Debug` traits with derive support.

### 10.2 `std::io`

- `Reader`, `Writer` traits.
- `BufReader`, `BufWriter`.
- `std::io::stdin()`, `stdout()`, `stderr()`.
- `copy(r: &mut dyn Reader, w: &mut dyn Writer) -> Result<u64, Error>`.

### 10.3 `std::os`

- `os::read_file(path: String) -> Result<Vec<u8>, Error>`.
- `os::write_file(path: String, bytes: &Vec<u8>) -> Result<(), Error>`.
- `os::open(path: String) -> Result<File, Error>`.
- `File` with `read`, `write`, `read_to_end`, `read_to_string`, `close`.
- `os::args() -> Vec<String>`.
- `os::env(key: String) -> Option<String>`.
- `os::exit(code: i32) -> !`.

### 10.4 `std::iter`

- `trait Iterator { type Item; fn next(&mut self) -> Option<Self::Item>; ... }`.
- Combinators: `map`, `filter`, `fold`, `collect`, `take`, `skip`,
  `zip`, `enumerate`, `chain`, `flat_map`, `any`, `all`, `sum`, `count`.

### 10.5 `std::strings` (alias `std::str`)

- Split, join, trim, contains, replace, find, lines, chars, bytes,
  to_lowercase, to_uppercase, starts_with, ends_with, repeat.

### 10.6 `std::strconv`

- `parse_i64(s: &String) -> Result<i64, ParseError>`
- `parse_f64`, `parse_bool`, etc.
- Formatting via `fmt::format!`.

### 10.7 `std::collections`

- `Vec<T>`, `HashMap<K, V>`, `BTreeMap<K, V>`, `HashSet<T>`,
  `BTreeSet<T>`, `VecDeque<T>`, `LinkedList<T>`.

### 10.8 `std::sync`

- `Mutex<T>`, `RwLock<T>` (parking_lot-style: no poisoning).
- `Once`, `WaitGroup`, `Barrier`.
- `atomic::{AtomicI32, AtomicI64, AtomicUsize, AtomicBool, AtomicPtr}`.

### 10.9 `std::time`

- `time::sleep(millis: u64)`.
- `Instant`, `Duration`.
- `SystemTime`.
- `time::now() -> SystemTime`.

### 10.10 `std::net` and `std::http`

- `net::TcpListener`, `TcpStream`, `UdpSocket`.
- `http::Server`, `http::Client`, `http::Request`, `http::Response`.
- `http::serve(addr: String, handler: impl Handler) -> Result<(), Error>`.

### 10.11 `std::encoding::json`, `std::encoding::csv`

- `json::encode(v: &T) -> Result<String, Error>` where `T: Serialize`.
- `json::decode::<T>(s: &String) -> Result<T, Error>` where
  `T: Deserialize`.
- `#[derive(Serialize, Deserialize)]` for automatic impls.

### 10.12 `std::thread`, `std::channel`

- `thread::spawn(|| { ... })` — OS thread (rarely used; prefer `go`).
- `channel<T>()`, `channel<T>(cap)`.

### 10.13 `std::panic`

- `panic!(msg: String)`.
- `catch_unwind(f: impl FnOnce() -> T) -> Result<T, PanicPayload>`.

---

## 11. Build and runtime

### 11.1 Targets

Supported from v1 (same set Go supports for static linking):

| OS × Arch | Backend |
|---|---|
| linux-x86_64 | Cranelift + LLVM |
| linux-aarch64 | Cranelift + LLVM |
| linux-riscv64 | LLVM (Cranelift support TBD) |
| darwin-x86_64 | Cranelift + LLVM |
| darwin-aarch64 | Cranelift + LLVM |
| windows-x86_64 | Cranelift + LLVM |
| freebsd-x86_64 | Cranelift + LLVM |
| wasm32-wasi | Cranelift (wasm backend) |

### 11.2 Linking

Static linking by default. The produced binary embeds the runtime and
the GC. On Linux, `musl` target produces a zero-libc static binary
identical in deployment experience to `CGO_ENABLED=0` Go.

Dynamic linking for FFI is supported via `extern "C"` blocks.

### 11.3 Compile modes

| Mode | Command | Backend | Speed | Output quality |
|---|---|---|---|---|
| Interpret | `gos run file.gos` | Bytecode VM | Fastest cold start | No native codegen |
| Debug build | `gos build` | Cranelift | Seconds for 100k LoC | ~2x slower than release |
| Release build | `gos build --release` | LLVM | Minutes for 1M LoC | Optimized |

### 11.4 Cross-compilation

```
gos build --target linux-aarch64 --release
```

All targets share the same frontend and MIR; only the backend pass
differs. Runtime libraries are prebuilt per-target and shipped with
the toolchain.

---

## 12. FFI

```
extern "C" {
  fn malloc(size: usize) -> *mut u8
  fn free(ptr: *mut u8)
}

#[no_mangle]
extern "C" fn my_exported(x: i32) -> i32 { x + 1 }
```

FFI rules:

- Types crossing the boundary must be FFI-safe: primitives, raw
  pointers, `extern "C" fn`, `#[repr(C)]` structs, FFI arrays.
- `String`, `Vec<T>`, and trait objects cannot cross FFI as values;
  callers pass `*const u8` + `usize` explicitly.
- Calls into FFI enter a scheduler state (`entersyscall`) that
  releases the P, so long-running C calls don't block other
  goroutines.

---

## 13. Attributes

```
#[derive(Debug, Clone, Serialize)]
#[inline]
#[no_mangle]
#[repr(C)]
#[cfg(target_os = "linux")]
#[test]
```

Only a curated set is recognized (unknown attributes warn rather than
error for forward-compatibility).

---

## 14. Macros

v1 supports only the built-in macros:

- `println!`, `print!`, `eprintln!`, `eprint!`, `format!`, `write!`,
  `writeln!`.
- `panic!`, `unreachable!`, `todo!`, `unimplemented!`.
- `vec!`, `map!`, `set!` (collection literals).
- `assert!`, `assert_eq!`, `debug_assert!`.
- `include_str!`, `include_bytes!`, `env!`.

User-defined macros (declarative `macro_rules!` or procedural) are
**post-v1**.

---

## 15. Grammar summary

A condensed top-level grammar:

```
SourceFile   = { UseDecl } { Item }
UseDecl      = "use" UseTarget [ "as" Ident ] [ "{" UseSpec "}" ]
UseTarget    = ProjectUse | ModulePath
ProjectUse   = StringLit [ "::" ModulePath ]
ModulePath   = Ident { "::" Ident }
Item         = FnDecl | StructDecl | EnumDecl | TraitDecl | ImplDecl
             | TypeAlias | ConstDecl | StaticDecl
             | ModDecl | AttrItem

FnDecl       = [Attrs] [ "pub" ] [ "unsafe" ] "fn" Ident [ Generics ]
               "(" [ ParamList ] ")" [ "->" Type ] [ WhereClause ] Block
ParamList    = Param { "," Param } [ "," ]
Param        = ( "self" | "&" "self" | "&" "mut" "self" | Pattern ":" Type )

Block        = "{" { Stmt ";" } [ Expr ] "}"
Stmt         = LetStmt | Item | ExprStmt | DeferStmt | GoStmt
ExprStmt     = Expr [ ";" ]

Expr         = LiteralExpr | PathExpr | CallExpr | MethodCall | FieldAccess
             | IndexExpr | UnaryExpr | BinaryExpr | AssignExpr | CastExpr
             | IfExpr | MatchExpr | LoopExpr | WhileExpr | ForExpr
             | BlockExpr | ClosureExpr | ReturnExpr | BreakExpr | ContinueExpr
             | TupleExpr | StructExpr | ArrayExpr | RangeExpr | UnsafeExpr
             | TryExpr | RefExpr | SelectExpr | MacroCall | PipeExpr

PipeExpr     = Expr "|>" PipeRhs
PipeRhs      = PathExpr                                  // x |> f
             | PathExpr "(" [ ArgList ] ")"              // x |> f(a, b)
             | Expr "." Ident                            // x |> obj.m
             | Expr "." Ident "(" [ ArgList ] ")"        // x |> obj.m(a)
             | "(" Expr ")"                              // x |> (closure)

Pattern      = (see §5)
Type         = (see §3)
```

Full grammar lives in `grammar/grammar.bnf` in the implementation
repository.

---

## 16. Project tool

The `gos` tool reads `project.toml` to resolve dependencies, fetch
sources, and drive the compiler. Resolution is tool-driven; the
language grammar knows nothing about networks, tarballs, or version
numbers.

### 16.1 Manifest

See §6.4. The only required keys are `project.id` and
`project.version`. Everything else is optional.

### 16.2 Sources

Four dependency source kinds (§6.7), in order of typical preference:

- **Registry** — HTTP endpoint serving signed tarballs, matched by DNS
  prefix via `[registries]`.
- **Git** — clone and check out a tag, branch, or rev.
- **Local path** — for side-by-side development of related projects.
- **URL tarball** — plain archive with mandatory sha256.

No source kind is privileged; all four interoperate.

### 16.3 Lockfile

`project.lock` is a TOML file capturing the exact resolution of every
transitive dependency: project identifier, concrete source, and source
tree sha256. Checked into version control for reproducible builds.

### 16.4 Version selection

Each dependency declares a semver **range** (default: `^x.y.z`). The
resolver picks the minimum version that satisfies all consumers
(minimum-version-selection, as in Go modules). This yields predictable
upgrades without surprise version jumps.

### 16.5 Caches

Content-addressable cache under `~/.gossamer/cache/<sha256>/`. Built
artifacts cached per target under
`~/.gossamer/build/<target>/<sha256>/`.

### 16.6 Subcommands

- `gos init` — create `project.toml` in the current directory.
- `gos new NAME` — scaffold a new project directory.
- `gos add ID[@VERSION]` — add a dependency entry.
- `gos remove ID` — remove a dependency entry.
- `gos build` — compile the current project.
- `gos run` — interpret the current project.
- `gos test` — run the project's tests.
- `gos fetch` — resolve and download (but do not build) all deps.
- `gos update` — update deps within their declared ranges.
- `gos tidy` — rewrite the manifest to its minimal closure.
- `gos vendor` — copy deps into `./vendor/`.
- `gos doc` — generate HTML documentation.

### 16.7 Reproducibility

Build output is a pure function of:

- Toolchain version.
- Target triple.
- Source file contents (current project plus all lockfile entries).
- Build flags (release/debug, features).

Backends are deterministic where possible (Cranelift fully; LLVM with
`-frandom-seed`).

### 16.8 Registries (optional)

A registry is a plain HTTP service that maps `/v1/<project-id>/<version>`
to a signed tarball plus metadata. Any party may run one. Projects
opt in by listing the registry's DNS prefix in `[registries]`:

```toml
[registries]
"acme.dev" = "https://registry.acme.dev/v1"
```

No central registry is shipped with the toolchain and none is required
to use Gossamer. A project whose dependencies are all git or path
sources is a fully supported, registry-free setup.

---

## 17. Versioning and compatibility

- Language version string: `edition = "2026"` in the manifest.
- The compiler accepts any edition up to its own.
- Breaking changes land only between editions.
- Minor/patch versions of the toolchain are backward-compatible.

---

## Appendix A — Differences from Go

1. No `nil`. All absence goes through `Option`.
2. No implicit zero-value for function types.
3. No interfaces in Go's sense — traits with explicit `impl`.
4. No `iota` — use `const` or an enum with explicit discriminants.
5. No type switch `x.(type)` — use `match` on an enum or `match` on
   a trait object with `Any::type_id`.
6. No labeled `goto`.
7. No implicit newline-as-semicolon rule.
8. Visibility by `pub`, not by capitalization.
9. Generics syntax is `<T>`, not `[T]`.

## Appendix B — Differences from Rust

1. No lifetimes (GC removes the need).
2. No borrow checker (GC removes the need).
3. No `Drop` trait with deterministic destruction — use `defer` for
   cleanup tied to scope; the GC reclaims memory.
4. No `Box<T>` / `Rc<T>` / `Arc<T>` — plain references are GC-managed
   and safe to share across goroutines.
5. `&T` is a GC reference, not a borrow. `&T` and `&mut T` have the
   same runtime; the distinction is a type-check-only aliasing hint.
6. No `async`/`await` — goroutines replace the entire async story.
7. No macros in v1 (beyond built-ins).
8. `go` and `select` keywords added.
9. `defer` keyword added.
10. **Forward pipe `|>`** (F#-style, left-associative, appends the
    piped value as the **last** argument). See §4.6. Rust has no pipe
    operator; Gossamer adds it as a first-class part of the grammar.

## Appendix C — Go features not ported

- `iota` (use `enum` discriminants).
- Embedded structs with method promotion (use explicit delegation or
  traits with default methods).
- `panic`/`recover` at function level (use `catch_unwind` at closure
  level).
- Init functions with ordering by import — replaced by explicit
  `fn init()` called in dependency-topological order.
- Untyped constants with arbitrary precision — literal constants have
  a default type and are coerced at use sites; infinite-precision
  compile-time arithmetic is not performed beyond what LLVM/Cranelift
  offer.
- `goto` — omitted.

---

*End of Gossamer specification (pre-1.0.0 draft).*
