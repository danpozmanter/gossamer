# Gossamer Language Specification

> Status: pre-1.0.0 draft. Models the current Gossamer language -
> a language targeting the 2026+ Rust ecosystem. The CLI toolchain is
> `gos` (single unified binary in the spirit of `go` or `cargo`).
> Source files use the extension `.gos`. The manifest file is
> `project.toml`; the lockfile is `project.lock`.

---

## 1. Introduction

Gossamer is a general-purpose, automatically memory-managed, statically
typed programming language with first-class concurrency. It runs on the
same target set as Go, compiles to a single self-contained binary, and
shares Go's runtime model (M:N goroutine scheduler, channels, automatic
memory management). Its surface syntax, type system, and error-handling
discipline are taken from Rust (2024 edition): `fn` declarations, `let`
bindings, `struct`/`enum`/`trait`/`impl`, pattern matching, `Option` and
`Result`, the `?` operator, monomorphized generics.

Gossamer deliberately omits:

- Lifetimes and the borrow checker (automatic memory management
  removes the need).
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
- **Expressiveness via functional combinators.** Character economy
  cuts ceremony out of programs; expressiveness keeps the remaining
  programs at the expression level instead of dropping them to
  procedural loops with named accumulators. The forward pipe `|>`
  (§4.6) is one half of this; a comprehensive `std::iter` /
  `std::option` / `std::result` surface (§10.4) is the other. The
  rule of thumb: a data transformation should read top-to-bottom as
  a chain of named stages, not as a `for` loop building up a `let
  mut` accumulator. `for` loops keep their place for side-effects
  and complex state; transformations should compose. Gossamer
  follows F#'s "data-last" argument-order convention in stdlib free
  functions so `x |> f(a, b)` reads as `f(a, b, x)` naturally.
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
is reserved but has no role - source files do not declare a package;
see §6. `move` is **not** a keyword: Gossamer has no ownership
transfer, so the Rust-style `move` closure qualifier would be
meaningless. Closures capture by managed reference for heap types and
by copy for `Copy` types with no opt-in needed.

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

Unlike Rust, Gossamer does not use `&` to mean "borrow" - `&expr` takes
a managed reference (see §4.3). As a prefix, `*expr` dereferences a
managed reference (`&T -> T`); it works anywhere, not only inside an
`unsafe` block. Regular method/field access auto-dereferences, so an
explicit `*` is rarely needed.

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

- `42i32`, `42u64`, `42usize` - typed integer literals.
- `3.14f32`, `3.14f64` - typed float literals.
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

One narrow newline rule disambiguates the three operators that are
also unary prefixes (`&`, `*`, `-`): when one of them appears as the
first non-whitespace token on a new line, it begins a new statement
rather than continuing the previous expression as a binary operator.
So:

```
let s = read_file(path)?
&s |> strings::lines |> iter::for_each(handle)   // two statements
```

parses as a let followed by a pipe-expression statement, not as
`let s = read_file(path)? & s |> ...`. Multi-line continuation of
those three operators still works when the operator sits at the end
of the previous line (`let x = a -\n  b`) or inside parentheses.
The other binary operators (`+`, `&&`, `|>`, `==`, …) continue across
newlines unconditionally.

> **Gotcha - leading `&` / `*` / `-` starts a new statement.** Because
> Gossamer never inserts semicolons, a line break before one of these
> three operators is **not** a continuation. Splitting a binary
> expression as
>
> ```
> let total = subtotal
>     - discount        // parsed as a new statement `-discount`, NOT a subtraction
> ```
>
> silently changes the meaning (here `total` binds to `subtotal` and
> `-discount` becomes a separate, discarded statement). Keep the
> operator at the **end** of the previous line, or wrap the expression
> in parentheses:
>
> ```
> let total = subtotal -
>     discount          // continues correctly
> let total = (subtotal
>     - discount)       // continues correctly
> ```

An entry file's top-level statements follow the same termination rules.
They form the body of an implicit `fn main` (§6.10) and have no
tail-expression value: a trailing bare expression is an ordinary
statement whose value is discarded, and the implicit `main` returns
`()` unless a `?` operator forces `Result<(), errors::Error>`.

---

## 3. Types

### 3.1 Built-in primitive types

- Signed ints: `i8`, `i16`, `i32`, `i64`, `i128`, `isize`.
- Unsigned ints: `u8`, `u16`, `u32`, `u64`, `u128`, `usize`.
  (`i128` / `u128` are reserved spellings, rejected with `GT0014` -
  see the conformance note below.)
- Floats: `f32`, `f64`.
- `bool` (1 byte).
- `char` - a 32-bit Unicode scalar value (not a surrogate).
- `()` - the unit type, inhabited by the value `()`.
- `!` - the never type (uninhabited; result type of `panic!`, `return`,
  infinite loops).

**The i64 runtime model.** Every integer type of 64 bits or less
(`i8`-`i64`, `u8`-`u64`, `isize`, `usize`) is represented at runtime
as a 64-bit signed value, on every tier. Arithmetic, comparison,
division, remainder, and shifts all run at 64-bit signed width; the
declared narrow or unsigned width is observable only at an explicit
`as` cast, which truncates to the declared width and then extends by
the target's signedness (`300 as u8 == 44`, `200 as i8 == -56`).
Consequences of the model:

- Arithmetic between narrow values does not wrap at the declared
  width: `200u8 + 200u8 == 400`. Wrap-at-width is opt-in via a cast
  (`(200u8 + 200u8) as u8 == 144`).
- `u64`/`usize` values at or beyond 2<sup>63</sup> are not
  representable distinctly - they alias the i64 two's-complement
  range, so `0u64 - 1` computes, compares, divides, and prints as
  `-1`, and `(0u64 - 1) as f64 == -1.0`. (Display provenance is the
  one exception: a value produced directly by an explicit `as u64` /
  `as usize` cast renders unsigned, so `(0 - 1) as u64` prints
  `18446744073709551615`.)
- `+`, `-`, `*` wrap two's-complement at 64-bit width.
- `<<` and `>>` mask the shift amount to the low 6 bits
  (`1 << 70 == 1 << 6`); `>>` is the arithmetic (sign-propagating)
  shift.
- Float → int casts saturate at i64 width with no narrow mask
  (`300.7 as u8 == 300`, `1e20 as i64 == i64::MAX`, NaN → 0).

No tier emits overflow traps: integer arithmetic wraps at 64-bit width
in every build mode, and there is no debug-mode overflow panic. The
method forms `checked_add`, `wrapping_add`, `saturating_add`,
`overflowing_add` and friends are the portable way to request explicit
overflow behaviour.

`i128` and `u128` are not supported on any tier. The checker rejects
every spelling of these types at the declaration site with a
compile-time error (`GT0014`), so `gos run`, the JIT, and `gos build`
all fail identically - there is no interpreter-only acceptance and no
silent 64-bit narrowing.

Silent surprise is never part of the contract - the behaviour is
explicit two's-complement arithmetic at 64-bit width, not
"undefined."

**No implicit numeric widening.** All numeric conversions - widening or
narrowing - require an explicit `as` cast. `let bigger: i64 = small_i32`
is a type error; write `let bigger = small_i32 as i64`. This prevents
silent truncation, silent sign changes, and surprise precision loss.

**The `as` whitelist.** `as` is whitelist-checked (`GT0005`). The
permitted shapes are: numeric ↔ numeric (any integer or float type on
either side, `f32` sources included; float → int truncates toward zero
and saturates as above), `bool` → integer, `char` → integer, `u8` →
`char`, and same-type no-ops. Every other `as` shape is a compile-time
error.

### 3.2 Strings

`String` is a runtime-managed, growable UTF-8 string. It is mutable
through `push_str`, `+`, and similar methods. Because `String` is
runtime-managed, there is no `&str`/`String` split and no lifetime
parameter. String literals have type `String`.

`char` is a 32-bit Unicode scalar value. A `String` is not indexable by
`char`; iteration is via `.chars()` (an iterator of `char`) or
`.bytes()` (an iterator of `u8`). Byte-level substring operations go
through `.as_bytes()` which returns `&Vec<u8>` sharing the underlying
bytes.

### 3.3 Collections (built-in generic types)

| Type | Semantics |
|---|---|
| `Vec<T>` | Growable slice. Analogue of Go's `[]T`. |
| `[T; N]` | Fixed-size array. Analogue of Go's `[N]T`. (There is no `Array<T, N>` spelling.) |
| `HashMap<K, V>` | Hash map. Analogue of Go's `map[K]V`. |
| `BTreeMap<K, V>` | Ordered map. |
| `HashSet<T>`, `BTreeSet<T>` | Sets. |
| `Sender<T>`, `Receiver<T>` | Channel endpoints. Always come as a pair from `channel<T>()`. |

All collections are managed reference types. Assigning
`let b = a;` where `a: Vec<T>` creates a second reference to the same
underlying slice (same as Go's behavior). For a deep copy, use
`.clone()`.

#### Collection literals

`[a, b, c]` (list) and `[value; count]` (repeat) are the literal forms
for sequences. With no contextual type they infer a fixed `[T; N]`
array; where the expected type is a growable `Vec<T>` or `[T]` slice,
the literal **coerces** to that growable form. Coercion fires at every
expected-type site:

```gossamer
let words: Vec<String> = ["yes", "wow"]      // let annotation
let zeros: Vec<i64> = [0; 4]                  // repeat form
fn names() -> Vec<String> { ["ada", "grace"] }   // return position
struct Basket { fruits: Vec<String> }
let b = Basket { fruits: ["apple", "pear"] }  // struct field
fn total(xs: Vec<i64>) -> i64 { /* … */ }
total([1, 2, 3])                              // by-value argument
fn count(xs: &[String]) -> i64 { xs.len() }
count(["m", "n"])                             // slice argument
```

An `if` or `match` whose branches are array literals of differing
length joins to `Vec<T>` - the only type that holds both - for every
element type (`if c { ["a", "b"] } else { ["c"] }` is a
`Vec<String>`). Equal-length branches stay a fixed `[T; N]` unless the
surrounding context asks for a Vec. Nested literals follow the element
type: `let g: Vec<Vec<i64>> = [[1, 2], [3]]` builds a Vec of Vecs.

### 3.4 Pointers and references

- `T` - a value. For `Copy` types (primitive numerics, `bool`, `char`,
  small POD structs) pass/return is a copy. For heap-managed types,
  `T` still names a managed reference - assignment and parameter
  passing create a second reference to the same heap cell. There is no
  ownership transfer and no `move` keyword: the original binding
  remains accessible.
- `&T` - a **shared managed reference** to a value of type `T`. Not a
  raw pointer. Cannot be null. Created by `&expr`. Auto-dereferenced
  for `.` access. Liveness is guaranteed by the runtime. `&` is an
  aliasing-intent marker; there is no compiler pass that enforces an
  aliasing discipline today (§7.5).
- `&mut T` - a **mutable managed reference**, used to signal write
  intent through a reference. Exclusivity is *not* enforced: no pass
  rejects two simultaneous `&mut` to the same value, and a `&mut T`
  struct field is accepted. Does not carry write-through across a `go`
  or channel boundary (see the parameter-semantics paragraph below).

Raw pointers (`*const T`, `*mut T`) are **not** part of the language
today: the type spellings do not parse (`GP0001`), and there is no safe
or unsafe way to construct one in Gossamer source. FFI goes through the
`gossamer-binding` ABI (§12), not raw pointers. (The `unsafe` keyword
parses - see §8.6 - but grants no extra powers, because there is nothing
unsafe to do.)

`&T` and `&mut T` in Gossamer are not borrows in the Rust sense -
automatic memory management already guarantees liveness. They are
aliasing-intent markers only; there is no borrow-checking or
exclusivity-enforcement pass today (§7.5). No lifetime parameters exist
at any level of the language.

**`&mut` parameter semantics.** A `&mut Vec<T>` / `&mut [T]` parameter
writes through to the caller's storage on every tier: element writes,
growth via `push` (a visible reallocation included), `swap`, forwarding
the reference into a nested call, early-return paths, a struct field as
the argument place, and writes from a closure taking the parameter are
all visible in the caller's binding after the call returns. Fixed-size
`[T; N]` arguments are copied at the call boundary - a fixed array
passed where a `&mut [T]` / `&mut Vec<T>` parameter is expected mutates
the copy, and write-through for fixed arrays is not part of the
contract. Passing the same place twice as `&mut` in a single call
(`f(&mut v, &mut v)`) is accepted (no exclusivity check, §7.5) and the
resulting write order is unspecified - do not rely on it. A `&mut`
argument in a `go` expression does not extend
write-through across the goroutine boundary: `go f(&mut v)` hands the
spawned call a copy, and programs must not rely on the goroutine's
writes being visible in the caller.

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
heap-allocated. Because there is no borrow checker, `Fn` and `FnMut`
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
may escape to the managed heap via escape analysis (any field mutation
through a `&T`, any storage in a channel, any capture by a closure that
outlives the caller, etc.).

Example:

```
pub struct Point { pub x: f64, pub y: f64 }
struct Wrapper(i32, i32);     // tuple struct
struct Marker;                // unit struct
```

**Functional record update.** A struct literal may spread a base value
with `..base` and override individual fields:

```
let p2 = Point { ..p1, x: 10.0 }     // x overridden, y copied from p1
let p3 = Point { x: 10.0, ..p1 }     // spread in any position
```

Explicit fields win over the base for the same name; exactly one `..base`
spread is allowed. Fields copied from the base share its heap children and
are retained, so the base stays usable after the update.

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

> **Constraint - variant names share the module namespace.** Unlike
> Rust, a variant name is not scoped under its enum: every variant in a
> module occupies the module's top-level name namespace. Two enums in
> the same module therefore cannot both declare a variant with the same
> name (`enum Color { …, C }` and `enum Grade { …, C }` collide with
> `GR0003: the name 'C' is defined multiple times`). Give colliding
> variants distinct names. This is also why method dispatch is largely
> name-global. (`Option` / `Result` are special-cased and do not
> reserve `Some` / `None` / `Ok` / `Err` against your enums.)

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

**Trait bounds on generic functions (static dispatch).** A generic
function may bound its type parameters by a trait and call the trait's
methods on a parameter receiver:

```
trait Shape { fn name(&self) -> String; fn area(&self) -> i64; }
fn report<T: Shape>(s: &T) -> String {
    format!("{}: {}", s.name(), s.area())
}
```

Each call site instantiates the type parameters independently, so one
generic function serves any number of concrete types in a program. The
bound is enforced at the call site: passing a type with no matching
`impl` is a compile error (`GT0017`). A method called on a bound
parameter resolves to the trait method's declared return type. Every
instantiation is monomorphised and the trait-method call is lowered to
the concrete impl's symbol (`Square::name`), giving static dispatch that
is bit-identical across the bytecode VM, the Cranelift JIT, and the LLVM
AOT tiers. The currently-supported surface is single-bound type
parameters with struct arguments; `dyn Trait`, operator traits,
associated-type projection in bounds, blanket impls, and supertrait
method inheritance through a bound are not yet part of static dispatch.

Gossamer does **not** support:

- Higher-ranked trait bounds (`for<'a> ...`).
- Trait objects of any kind. There is no `dyn Trait` (§3.11), so
  object-safety / dyn-compatibility rules do not apply - polymorphism
  is monomorphised generic bounds only.

### 3.9 Impl blocks

```
ImplDecl      = "impl" [ Generics ] Type [ WhereClause ] "{" ImplItems "}"
ImplDeclTrait = "impl" [ Generics ] TraitRef "for" Type [ WhereClause ] "{" ImplItems "}"
```

Inherent impls attach methods/associated items to a type. Trait impls
declare that a type satisfies a trait.

Method receivers:

- `fn m(self)` - receives the value by copy (Copy types) or by
  managed reference (heap types). The caller's binding remains
  usable after the call.
- `fn m(&self)` - shared access. With managed references this is
  just "pass the ref".
- `fn m(&mut self)` - exclusive access. Same runtime as `&self`; used
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
mirror Rust crates - they are parsed and ignored by the type checker in
safe code. In normal code, lifetimes are never written.

Generic instantiation in expressions uses the turbofish `::<T>`:

```
let v = Vec::<i32>::new()
let (tx, rx) = channel::<String>()
```

The bare form `name<T>(...)` is also accepted when the parser can
disambiguate with one-token lookahead after the closing `>` (must be
`(`, `::`, or `{`).

A const parameter over an array length binds the length of a fixed-size
array parameter:

```
fn sum<const N: usize>(xs: [i64; N]) -> i64 {
    let mut acc = 0
    for x in xs { acc += x }
    acc
}
```

`N` is inferred from the array argument's length at the call site and
keyed into monomorphisation, so each distinct length instantiates an
independent specialisation that runs identically on the bytecode VM,
the Cranelift JIT, and the LLVM AOT tiers. The body may iterate the
parameter and read `xs.len()`, the const may appear in the return type
(`-> [i64; N]`), and a function may take more than one const parameter
(`<const N: usize, const M: usize>`). The const is inferred from a
`[T; N]` argument; it is not yet usable as a bare value expression in
the body or as a repeat count (`[0; N]`).

Monomorphisation specialises each `(def, substs)` pair independently
and runs identically on the bytecode VM, the Cranelift JIT, and the
LLVM AOT tiers. A generic parameter may be instantiated with a scalar
or with an aggregate - a struct, tuple, fixed-size array, `Vec<T>`,
`String`, or `f64` - and the aggregate is threaded by value through the
specialisation, including across recursive calls. Generic struct types
(`struct Wrapper<T> { value: T }`) and their `impl<T>` methods
specialise per instantiation on every tier. Bounds are single-bound
static dispatch: there is no `dyn Trait`, no operator-trait or
associated-type bound, and no supertrait method inheritance through the
bound.

### 3.11 Dynamic dispatch

There is no `dyn Trait` and no trait-object type. `dyn` is not a
reserved word, and the `dyn Trait` type spelling does not parse
(`GP0001`). Polymorphism is provided by generic bounds with static
dispatch (§3.10): `fn f<T: Trait>(x: &T)` monomorphises per call site.
A heterogeneous collection is modelled with an `enum` whose variants
carry the alternatives, matched exhaustively.

For closures, the callable trait type `Fn(args) -> ret` (§3.5) is the
one place a value of "some callable" is passed dynamically; it is a fat
pointer, but it is not spelled `dyn`.

### 3.12 Type aliases

```
TypeAlias = "type" Ident [ Generics ] "=" Type ";"
```

### 3.13 Derivable traits

A struct may be annotated with `#[derive(...)]` to have standard trait methods
generated automatically:

```
#[derive(PartialEq, Eq, Default, Debug)]
struct Point { x: i64, y: i64 }
```

The supported traits are `Debug`, `Default`, `PartialEq`, `Eq`, `PartialOrd`,
and `Ord`:

- `PartialEq` / `Eq` - `==` and `!=` compare field-by-field. (`Eq` is a marker
  requiring `PartialEq`.) Note structs / enums already compare by value with no
  derive (§3.12); the derive forces it for generic / container-field types.
- `PartialOrd` / `Ord` - `<` `<=` `>` `>=` order field-by-field (structs) or by
  variant rank then payload (enums); likewise automatic for plain types.
- `Default` - `Type::default()` builds a zero-valued instance (`0` / `false` /
  `""` / `[]` / each field type's own default; skipped when a field type has no
  derivable default).
- `Debug` - `{:?}` / `{}` render `Name { field: value, … }`.

`Clone` is **not** derivable (`GT0025`): structs copy by value, so `let b = a`
copies and `a.clone()` is a universal builtin. `Hash`, `Copy`, `Display`, and
serde are likewise automatic; `From` / operators are written `impl Trait for T`.

The methods are synthesized as ordinary Gossamer `impl` source at parse time,
so they compile and run identically on every tier. Fields may be primitives,
`String`, `[T]`, **nested structs** (which derive the same traits), and the
struct may be **generic** (`struct Wrap<T> { … }`).

`#[derive(...)]` also works on **enums**, including variants with
struct payloads. Tuple (`Circle(f64)`), unit (`Point`), and
struct-payload (`Rect { w, h }`) variants may be mixed freely:
`Clone`, `PartialEq` / `Eq`, `Debug` (`Rect { w: 2, h: 3 }`), and
`Default` (which selects the `#[default]` unit variant) all derive and
run identically on every tier.

A struct / tuple used as a `HashMap` / `HashSet` key is hashed and compared by
value on every tier - so two equal-valued keys at distinct allocations resolve
to the same entry - with no `#[derive(Hash)]`; hashing is automatic, and
`#[derive(Hash)]` is rejected (`GT0025`).

---

## 4. Variables, expressions, statements

### 4.1 Bindings

```
LetStmt = "let" [ "mut" ] Pattern [ ":" Type ] [ "=" Expr ] ";"
```

- `let x = 1` - immutable binding, type inferred.
- `let mut x = 1` - mutable binding.
- `let (a, b) = pair` - destructuring.
- `let Point { x, y } = p` - struct destructuring.
- `let Point { x: a, y: b } = p` - renamed struct destructuring.
- `let Nested { p: Point { x, y }, label } = n` - nested struct.
- `let Shape::Pair(m, n) = s` - enum / tuple-struct variant.
- `let (A(g, _) | B(g)) = v` - or-pattern (alternatives must bind the
  same names).
- `let x: i64 = 1` - annotated.

A `let` pattern must be irrefutable (it always matches); a refutable
pattern requires `let ... else { ... }` (the `else` block must diverge).
Irrefutable struct, nested-struct, variant, and or-pattern destructuring
bind correct values on the bytecode VM, the Cranelift JIT, and the LLVM
AOT tiers.

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

`&expr` creates a managed reference. `&mut expr` is the same runtime
but requires `expr` to be a mutable place. In the absence of lifetimes
and borrow checking, this is pure ergonomics.

`*expr` dereferences a managed reference (`&T -> T`); it is not
restricted to `unsafe` and there are no raw pointers to dereference.
Regular `&T -> T` dereference is also implicit at `.` and index
operators, so an explicit `*` is rarely needed.

### 4.4 Control flow

#### `if`

```
IfExpr      = "if" Condition Block [ "else" ( IfExpr | Block ) ]
Condition   = CondClause { "&&" CondClause }
CondClause  = "let" Pattern "=" Expr | Expr
```

An `if` without an `else` has type `()`. With `else`, both arms must
produce the same type (or one is `!`).

A condition is a let-chain: a sequence of clauses joined by `&&`, where
each clause is either a `let` binding (`let PAT = expr`, which matches a
refutable pattern against a scrutinee) or a boolean expression. The chain
holds when every clause holds: each `let` clause must match and each
boolean clause must be `true`. Earlier `let` bindings are in scope for
every later clause and for the body and `else` branch.

```
if let Some(x) = a && let Some(y) = b && x > 0 {
  use(x + y)
}
```

A `let` clause chain is `&&`-only: joining `let` clauses with `||`
without parentheses is a parse error (`GP0001`). The construct is a
front-end desugar into nested `match`, so it runs identically across all
tiers.

#### `match`

```
MatchExpr = "match" Expr "{" MatchArm { "," MatchArm } [ "," ] "}"
MatchArm  = Pattern [ "if" Expr ] "=>" ( Expr | Block )
```

`match` is exhaustive. Non-exhaustive `match` is a compile error.
Patterns support literals, wildcards (`_`), ranges, bindings,
struct/enum destructuring, and or-patterns (`A | B`). Ranges may be
closed (`1..=10`), exclusive (`1..10`), or open-ended - `..=hi` and
`..hi` (open start) and `lo..` and `lo..=` (open end, covering up to
the type maximum inclusive). Range patterns are opaque to exhaustiveness
analysis, so a `_` arm is still required even when the ranges appear to
cover the type; `..=` with no upper bound is a parse error.

```
match divide(a, b) {
  Ok(v) => println!("got: {}", v),
  Err(e) => eprintln!("err: {}", e),
}
```

#### `while`, `loop`, `for`

```
WhileExpr  = "while" Condition Block
LoopExpr   = "loop" Block
ForExpr    = "for" Pattern "in" Expr Block
```

A `while` condition is the same let-chain form as `if` (see above): a
sequence of `let PAT = expr` and boolean clauses joined by `&&`, with
earlier bindings in scope for later clauses and the body. The loop runs
while the whole chain holds.

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

#### `arena`

```
ArenaStmt = "arena" Block
```

`arena` is a contextual keyword (an identifier `arena` not followed by
`{` is an ordinary name). Every allocation made while the block runs is
bump-allocated into a fresh arena and freed wholesale when the block
exits, on every exit path - the construct desugars to
`runtime::arena_push()` plus a block-scoped
`defer runtime::arena_pop()`. The block is statement-position only and
yields `()`; a tail expression is evaluated and discarded.

Contract: a value allocated inside the block must not be referenced
after the block exits (no assignment to outer bindings, no stores into
outliving containers, no channel sends, no captures that outrun the
block). `Weak` references to arena values upgrade to `None`. Arenas
nest; inner arenas free at their own close brace.

#### `defer`

```
DeferStmt = "defer" Expr
```

`defer` is **block-scoped**, following Swift and Zig rather than Go: a
deferred expression runs when control leaves its *enclosing block* - by
falling off the end, `return`, `break`, or `continue` - not when the whole
function returns. Within a block, deferred expressions run in LIFO order. The
argument is any expression, commonly a call or a `{ }` block.

```
fn read_all(path: String) -> Result<Vec<u8>, Error> {
  let file = os::open(path)?
  defer file.close()      // runs when this function's block exits
  file.read_to_end()
}
```

Because the scope is the nearest `{ }`, a `defer` inside a loop body runs at
the end of *each* iteration:

```
while let Some(conn) = listener.accept() {
  defer conn.close()      // closed at the end of every iteration
  handle(conn)
}
```

Deferred expressions are **evaluated when they run**, not when registered:
they read the current value of any variable they reference at block exit (the
same capture rule as Swift/Zig). A deferred expression's own value and any
control flow inside it are discarded; a panic raised inside one propagates.

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

#### `spawn` / `join`

`spawn(f)` runs the callable `f` (a function or closure taking no
arguments) on a goroutine and returns a `JoinHandle<T>`, where `T` is
`f`'s return type. `handle.join()` blocks until the goroutine finishes
and yields its outcome as `Result<T, String>`: `Ok(value)` on a normal
return, or `Err(message)` if the goroutine panicked. Unlike `go`, which
is fire-and-forget, `spawn` lets the caller recover the result and
isolate a panic without ending the process.

```
let h = spawn(|| compute())
match h.join() {
    Ok(v)  => use(v),
    Err(e) => report(e),
}
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
  Ok(msg) = rx_ok.recv() => println!("success: {}", msg),
  Err(err) = rx_err.recv() => println!("error: {}", err),
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

The HIR desugar tracks the enclosing function's declared return
type so the cross-type conversion is automatic. When the inner
expression's `Err` type differs from the enclosing function's
`Err` type the desugar inserts a call to `errors::Error::from`
(or any user-supplied `Into<E2>` impl when the typechecker
recognises one) on the propagated value. Result and Option are
disambiguated by inspecting the inner type's ADT name; ambiguity
falls back to the Result desugar. See
`feature-testing-examples/try_option_propagation.gos` and
`feature-testing-examples/try_err_conversion.gos` for end-to-end
coverage across all three tiers.

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
// Equivalent to: println(format!("hello {}", name))
name |> format!("hello {}", ..) |> println
```

Note: the `..` placeholder is **not** required - `|>` implicitly
targets the last position. The code above would equivalently be
written:

```
name |> format!("hello {}", _) |> println   // _ is optional, sugar
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

**Argument-order convention.** Stdlib free functions intended to be
piped into (`std::iter`, `std::option`, `std::result`, and most of
`std::strings`) follow a uniform "data-last" rule: the value being
transformed is the **last** positional parameter. This is what makes
`x |> f(a, b)` thread cleanly without explicit placeholders. The
convention is documented per-module; APIs that diverge from it (for
historical or readability reasons) are called out at their declaration.

Interaction with `?`:

```
read_file(path)? |> parse_json::<Config>()?
```

Here `?` binds tighter than `|>` (§4.7 precedence), so this parses as
`(read_file(path)?) |> (parse_json::<Config>()?)` - the inner `?`
unwraps the `Result<String, _>`, pipes the `String` into
`parse_json`, and the outer `?` unwraps that result.

### 4.7 Operators and precedence

From highest to lowest:

| Level | Operators | Associativity |
|---|---|---|
| 1 | `::` path | left |
| 2 | `.` method/field, `[]`, `()`, `?`, postfix | left |
| 3 | unary `-`, `!`, `&`, `&mut`, `*` (deref) | right |
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
        | Literal ".." Literal               // range, exclusive
        | Literal "..=" Literal              // range, inclusive
        | ".." Literal                       // open-start, exclusive
        | "..=" Literal                      // open-start, inclusive
        | Literal ".."                       // open-end, exclusive
        | Literal "..="                      // open-end, inclusive
        | "&" Pattern                        // ref pattern
        | "mut" IdentPattern                 // mutable binding
        | ".." Pattern?                      // rest pattern
```

An open-ended range covers up to the type's maximum (inclusive). `..=`
with no upper bound is a parse error. Range patterns are opaque to the
exhaustiveness checker, so a match using only ranges still needs a `_`
arm.

A `let` binding (§4.1) and a `let` clause in an `if` / `while` condition
(§4.4) require an irrefutable pattern (or an `else` branch that diverges,
for `let ... else`). Struct, nested-struct, variant, and or-pattern
destructuring in irrefutable position bind identically across all tiers.

Exhaustiveness is checked via matrix decomposition (the Maranget
algorithm, same as Rust).

---

## 6. Projects, modules, and source files

Gossamer cleanly separates two concepts that other languages often
conflate:

- **Module** - how code is *organized* into namespaces. No version, no
  owner, no network identity.
- **Project** - how code is *distributed* and *versioned*. Carries a
  stable domain-based identifier, a semver, and dependency
  declarations.

A project contains one module or many. A module never spans projects.

### 6.1 Source files

A source file is plain Gossamer; it does not declare a package, does
not declare its module, and contains no boilerplate header.

```
SourceFile = { UseDecl } { Item | Statement }
```

A file's module is determined by its location on disk (§6.3). Its
owning project is determined by the nearest enclosing `project.toml`
walked upward from the file (§6.4).

Bare `Statement`s at file scope are accepted only in the entry file,
where they form the body of an implicit `fn main` (§6.10).

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

- `project.id` - the project identifier (see §6.5).
- `project.version` - semver `MAJOR.MINOR.PATCH`.

Every other key is optional.

Optional keys include `project.output` (binary name override) and
`project.entry` (path to the entry source, relative to the manifest
directory), which overrides convention-based entry resolution.

### 6.5 Project identifiers

A project identifier is a stable, location-independent string of the
form:

```
ProjectId = DomainSegment { "/" PathSegment }
DomainSegment = Label { "." Label }        // must contain at least one "."
Label         = [a-z][a-z0-9-]*
PathSegment   = [a-z0-9][a-z0-9-_]*
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
  may publish under a prefix - disputes are resolved by consumers
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

// Same-project paths use ordinary module syntax - no string.
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

An executable program's entry is `fn main`, returning either `()` or
`Result<(), E>`.

The entry file may omit the `fn main` wrapper and instead contain bare
statements at file scope: the entry file is then **implicitly wrapped in
`fn main()`**. The compiler collects the file's top-level statements, in
source order, as the body of a synthesized `fn main`; functions, structs,
and other items declared alongside them are hoisted to file scope as usual.
If any statement uses the `?` operator, the synthesized signature is
`fn main() -> Result<(), errors::Error>`; otherwise it returns `()`. Set a
process exit code explicitly with `std::process::exit(n)`.

A file may use exactly one entry form. Mixing bare top-level statements with
an explicit `fn main` is an error. Top-level statements are accepted only at
the entry file's top level, never inside a `mod { }` body.

The entry file is `src/main.gos` by convention, or whatever `[project] entry`
names (§6.4); a file passed directly to `gos run` / `gos build` is the entry.

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

All heap-allocated values are managed automatically by the runtime.
Values that do not escape their defining function may be
stack-allocated (escape analysis). The escape rules are:

1. Any value whose address is taken (`&x`) and passed across a call
   boundary escapes.
2. Any value assigned to a field of a heap-allocated struct escapes.
3. Any value sent on a channel escapes.
4. Any value captured by a closure that is stored or passed beyond the
   creating scope escapes.

### 7.2 Automatic memory management

Memory management is deterministic reference counting for heap enums
and runtime containers, drop-pass reclamation for value aggregates,
weak references, an on-demand cycle collector
(`runtime::collect_cycles()`), and `arena { }` regions - on every tier.
There is no tracing collector: no pacer, no write barrier, and no GC
pause.

Memory is reclaimed deterministically, without a tracing collector:

- **Reference counting.** Recursive heap enums and runtime
  containers carry an intrusive header (`[RcHeader | payload]`).
  Codegen emits balanced retain/release pairs; when the strong
  count reaches zero the value's reference-counted children are
  released iteratively and the payload is freed. Semantics match
  the interpreter tier's shared-ownership model.
- **Weak references.** A weak reference does not contribute to the
  strong count; upgrading after the payload is destroyed yields
  `None` (Swift-ARC model).
- **Cycle collection.** Reference cycles are reclaimed on demand by
  `runtime::collect_cycles()` (Bacon-Rajan trial deletion). No
  background collector runs.
- **Value aggregates.** Structs, tuples, and arrays are
  heap-allocated at construction and freed by the compile-time drop
  pass at scope exit. An aggregate that escapes the drop pass's
  analysis (e.g. stored through an opaque chain) is leaked until
  process exit rather than unsoundly collected.
- **Arenas.** `arena { }` bump-allocates everything constructed
  inside the block and frees it wholesale at every exit path
  (§4.4). Nothing allocated inside may be referenced after the
  block.

Safe points, pacers, and collection-triggered pauses do not exist;
allocation cost is a pointer bump (arena) or one `malloc`/refcount
(heap values).

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
- Atomics via `std::sync::atomic` (sequentially consistent by default;
  relaxed/acquire/release available).

A runtime data-race detector ships behind `gos test --race`. When it
is enabled, the LLVM AOT codegen instruments heap loads and stores with
`gos_rt_race_access` calls and the runtime (`gossamer-runtime::race`)
maintains a per-goroutine vector-clock happens-before model, recording
synchronisation edges at channel handoff, mutex unlock, and WaitGroup
done. Any access pair left unordered by a happens-before edge is
reported and fails the test run. It is a testing instrument rather than
an always-on runtime guard, and it sees the compiled-tier accesses the
codegen instruments; the `Once::call_once` and atomic-load/store
happens-before edges are not yet folded into the model.

### 7.5 References and aliasing (no borrow checker)

Gossamer has no ownership transfer, no `move` keyword, and no lifetime
annotations anywhere. All bindings stay live and accessible for the
duration of their lexical scope. Assignment, parameter passing, and
closure capture all behave like Go: a managed reference for
heap-managed types, a copy for `Copy` types. The cognitive load of "who
owns this value now" does not exist.

**There is no borrow checker and no aliasing-enforcement pass.** `&T`
and `&mut T` are *aliasing-intent markers* only - they document whether
a reference is used to read or to write. No compiler pass tracks
reference activity, rejects two simultaneous `&mut`, or forbids `&mut`
where a `&` is live. Programs that would violate a Rust-style
exclusivity rule compile and run on every tier; `&mut counter` twice in
a row is accepted, and a `&mut T` struct field is accepted.

Liveness is the one guarantee that *is* enforced - by automatic memory
management, at runtime, not by a static check. No reference dangles
because the runtime keeps every reachable value alive. Mutating a
collection while iterating it, self-aliasing, and the other patterns a
borrow checker would reject are the programmer's responsibility today;
they are not diagnosed.

There is no exclusivity-enforcement pass. `&mut T` is parsed and
type-checked, but no compiler pass rejects aliasing: a program that
would violate a Rust-style exclusivity rule compiles and runs on every
tier. A scope-local exclusivity check (many `&T`, or one `&mut T`,
never both) remains a candidate for a future release (§7.5.2), and
nothing in this specification depends on it.

#### 7.5.1 What this means in practice

- Use `&` to signal "I only read this" and `&mut` to signal "I write
  through this." These markers aid the reader and select the mutating
  vs non-mutating method where dispatch distinguishes them; they do not
  gate compilation.
- `&mut Vec<T>` / `&mut [T]` parameters do carry write-through to the
  caller (§3.4), so the marker is load-bearing for that data flow even
  though exclusivity is unchecked.
- Returning a reference from a function is permitted (the runtime keeps
  the pointee alive); the caller receives an unconstrained `&T` /
  `&mut T`.
- `go` and `Sender::send` pass values the same way ordinary assignment
  does - managed reference for heap types, copy for `Copy` types - and
  the caller retains access to its bindings (§8.1, §8.2). Cross-
  goroutine data races on shared mutable state are possible; prevent
  them by communicating through channels.

#### 7.5.2 What this deliberately omits

This design deliberately does *not* include:

- A borrow checker, region inference, or any aliasing-exclusivity pass.
- Explicit lifetime annotations (`'a`, `'static`, `for<'a>`). Lifetime
  syntax in a generic parameter list is parsed and then ignored.
- `Send`/`Sync` marker traits. The language accepts Go-style sharing
  discipline instead: communicate via channels; races on raw shared
  state are the programmer's responsibility, not blocked at compile
  time.

A scope-local exclusivity check (many `&T`, or one `&mut T`, never
both) remains a candidate for a future release, but it is not part of
the language today and nothing in this specification depends on it.

---

## 8. Concurrency

### 8.1 Goroutines

A goroutine is a stackful coroutine scheduled cooperatively by the
runtime. `go expr` spawns one. Each goroutine owns a fixed-size
mmap'd stack (default 16 KiB; override with `GOSSAMER_GOROUTINE_STACK`).
The stack lives below a guard page, so overflow traps as a
deterministic SIGSEGV instead of clobbering arbitrary memory.

**Argument discipline.** `go` captures values by managed reference the
same way an ordinary closure does. `Copy` types (primitive numerics,
`bool`, `char`, small POD structs) are captured by value. After
`go f(x)` returns, the caller may continue to use `x` - Gossamer has no
ownership transfer, no `move` keyword, and no "value becomes invalid
after this point" semantics. This matches Go.

A `go` call may not capture or pass a `&T` or `&mut T`. The tracked
`&`/`&mut` access markers are scope-local (§7.5) and cannot be
reasoned about across goroutine boundaries; permitting them would
either require lifetime annotations (which we explicitly avoid) or
silently weaken the local check. Pass the underlying value (managed
reference, or `Copy`) instead.

Cross-goroutine data races on shared mutable state are possible - the
same trade-off Go makes. Detect them at runtime with `gos test --race`
(§7.4) and prevent them by communicating through channels rather than
sharing state.

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

- `tx.send(v)` - blocks until a receiver is ready (unbuffered) or
  buffer has capacity.
- `rx.recv() -> Option<T>` - blocks until a sender sends a value or
  the channel is closed; returns `None` when the channel is closed and
  drained.
- `rx.try_recv() -> Option<T>` - non-blocking.
- `tx.close()` - marks the channel closed. Subsequent sends panic.
  Receives drain buffered values then return `None`.

Channels are many-to-many. Close only once.

`Sender::send` passes a managed reference for heap-managed types and a
copy for `Copy` types - the same rules as ordinary assignment or
function call. No ownership transfer is implied; the sender retains
access to whatever bindings it named. Sending a `&T` or `&mut T` on a
channel is a compile error, for the same reason `go` cannot capture
references: the local borrow check (§7.5) is scope-local and cannot
follow an access marker across goroutine boundaries.

### 8.3 Select

Each arm of `select` is a communication operation. The `rx.recv()`
call returns `Option<T>`, so matching `Some(v)` / `None` handles the
closed-channel case:

```
select {
  Some(v) = ch.recv() => process(v),
  None    = ch.recv() => { break; }      // channel closed
  default              => do_other(),
}
```

### 8.4 `defer` and goroutines

Deferred expressions are **block-scoped** (see §`defer`): each runs when
control leaves its enclosing `{ }` block, not when the whole function or
goroutine unwinds. As a goroutine's stack unwinds - whether by normal return
or by a panic - every block it leaves runs that block's pending defers in LIFO
order. A panic that is not recovered inside the goroutine ends that goroutine
(its defers still run as the stack unwinds); a panic on the main goroutine
crashes the process.

### 8.5 `recover`

`std::panic::catch_unwind(|| { ... })` returns `Result<T, PanicPayload>`,
catching panics inside the closure. This replaces Go's `recover()`.

### 8.6 `unsafe`

```
unsafe { ... }
unsafe fn raw_thing() { ... }
```

`unsafe { ... }` blocks and `unsafe fn` declarations **parse and run**,
but `unsafe` grants no additional powers today: there are no raw
pointers (§3.4), so there is nothing unsafe to do. An `unsafe { ... }`
block evaluates exactly like an ordinary block expression, and calling
an `unsafe fn` needs no `unsafe` ceremony. The keyword is accepted for
Rust source compatibility and as a forward-compatible marker.

`unsafe` never disables automatic memory management or affects memory
reclamation.

Source-level `extern "C"` items are **not** an `unsafe` power - they
are rejected at parse time (`GP0016`). The sole FFI surface is the
`gossamer-binding` ABI. See §12.

---

## 9. Error handling

Errors are values of types implementing the `Error` trait. Because
there is no `dyn Trait` (§3.11), the cause chain is walked through the
concrete `errors::Error` accessors rather than a `&dyn Error`:

```
pub trait Error: Display + Debug {
  fn message(&self) -> String          // this error's own message
  fn cause(&self) -> Option<Error>     // next link in the chain, if any
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

> **Implementation status (pre-v1):** The `must_use` lint for `Result`
> is not yet emitted. Silently-dropped `Result` values compile without
> warning today; the lint will be added before v1.0.0.

---

## 10. Standard library

This is an outline; full API docs ship with the first implementation.

### 10.1 `std::fmt`

- `println(args...)` - variadic print-with-newline. Each argument
  implements `Display`.
- `eprintln(args...)`.
- `format!(fmt_str, args...)` - returns `String`. `fmt_str` is a
  compile-time-validated format string (`{}` placeholders).
- `print`, `eprint` without newline.
- `Display`, `Debug` traits with derive support.

### 10.2 `std::io`

- `Reader`, `Writer` traits.
- `BufReader`, `BufWriter`.
- `std::io::stdin()`, `stdout()`, `stderr()`.
- `copy<R: Reader, W: Writer>(r: &mut R, w: &mut W) -> Result<u64, Error>`
  (static dispatch via generic bounds; there is no `dyn`, §3.11).

### 10.3 `std::os`

- `os::read_file(path: String) -> Result<Vec<u8>, Error>`.
- `os::write_file(path: String, bytes: &Vec<u8>) -> Result<(), Error>`.
- `os::open(path: String) -> Result<File, Error>`.
- `File` with `read`, `write`, `read_to_end`, `read_to_string`, `close`.
- `os::args() -> Vec<String>`.
- `os::env(key: String) -> Option<String>`.
- `os::exit(code: i32) -> !`.

### 10.4 `std::iter`

**One obvious way.** Transformations (`map`, `filter`, `fold`,
`reduce`, `partition`, …) live as **free functions in `std::iter`
only**. `Vec<T>`, `HashMap<K, V>`, `HashSet<T>`, `BTreeMap<K, V>`,
`Receiver<T>`, and friends do **not** carry `.map(…)` / `.filter(…)`
/ `.fold(…)` methods. F#'s `Seq`/`List`/`Array` module convention
applies: data flows through `|>` into free functions; the surface
stays small and the call shape is uniform. The mutating helpers
that don't compose with `|>` (`xs.push`, `xs.pop`, `xs.sort`,
`xs.swap`, `m.inc`, `m.or_insert`, etc.) remain methods because
they operate by side-effect on the receiver - there is no chain
to fit them into.

The iterator protocol is one trait. User code declares it as
needed (or any name; the for-loop dispatch only checks for a
`.next() -> Option<T>` method on the receiver):

```
trait Iterator {
    fn next(&mut self) -> Option<i64>  // pick a concrete item type
}
```

Associated types (`type Item`) parse but the typechecker
currently does not project through them; declare the item
type concretely (`Option<i64>`, `Option<String>`, …) for now.

Any type providing a `next(&mut self) -> Option<T>` method
ranges with `for`:

```
for x in iter { body }
```

The HIR desugars to

```
{
    let mut __for_iter = iter
    loop {
        match (&mut __for_iter).next() {
            Some(x) => body,
            None => break,
        }
    }
}
```

- binding the iter once outside the loop so each `.next()`
call advances the same state. `IntoIterator` is implicit:
`Vec<T>`, `HashMap<K,V>`, ranges, `Receiver<T>`, and any
user struct with `fn next(&mut self) -> Option<T>` are all
iterable directly.

Built-in iterator types (lazy, single-pass unless noted):
`RangeIter` (from `a..b` and `a..=b`), `VecIter`, `MapIter`,
`FilterIter`, `FilterMapIter`, `TakeIter`, `SkipIter`,
`TakeWhileIter`, `SkipWhileIter`, `ChainIter`, `ZipIter`,
`EnumerateIter`, `FlatMapIter`, `WindowsIter`, `ChunksIter`,
`PairwiseIter`, `RepeatIter`, `CycleIter`, `StepByIter`,
`ScanIter`, `UnfoldIter`.

Free functions in `std::iter`. Argument order is **data-last**
(§4.6) so every entry threads with `|>`. Functions taking a callable
accept any `Fn(...) -> _` (capturing closures and bare functions
both qualify).

Transformations (lazy when chained - no intermediate `Vec`
allocation between stages):

- `map(f, it)` - apply `f` to every element.
- `filter(p, it)` - keep elements where `p(elem)` is true.
- `filter_map(f, it)` - apply `f`, keep the `Some` results.
- `take(n, it)` / `skip(n, it)`.
- `take_while(p, it)` / `skip_while(p, it)`.
- `chain(a, b)`, `zip(a, b)`, `enumerate(it)`.
- `flat_map(f, it)`, `flatten(it)`.
- `windowed(n, it)`, `pairwise(it)`, `chunk_by_size(n, it)`.
- `step_by(n, it)`, `cycle(it)`, `repeat(v, n)`.
- `scan(init, f, it)`, `unfold(seed, f)`.

Consumers (force the chain to completion, return a non-iterator):

- `for_each(f, it)` - apply `f` for its side-effects; returns `()`.
- `fold(init, f, it)`, `reduce(f, it) -> Option<T>`.
- `sum(it)`, `sum_by(f, it)`, `product(it)`, `product_by(f, it)`.
- `count(it)`.
- `min(it) -> Option<T>`, `max`, `min_by`, `max_by`, `min_by_key`,
  `max_by_key`.
- `any(p, it)`, `all(p, it)`.
- `find(p, it) -> Option<T>`, `position(p, it) -> Option<usize>`,
  `find_map(f, it) -> Option<U>`.
- `partition(p, it) -> (Vec<T>, Vec<T>)`.

Materializers (consumers that build a collection):

- `collect::<C>(it)` - into `Vec<T>`, `HashSet<T>`, `HashMap<K,V>`,
  `BTreeMap<K,V>`, `String`, depending on the turbofish.
- `group_by(key, it) -> HashMap<K, Vec<T>>`.
- `count_by(key, it) -> HashMap<K, i64>`.
- `sort_by(cmp, it) -> Vec<T>`, `sort_by_key(key, it) -> Vec<T>`.
- `dedup(it) -> Vec<T>`, `reversed(it) -> Vec<T>`.
- `unzip(it) -> (Vec<A>, Vec<B>)`.

Constructors:

- `range(a, b) -> RangeIter`, `range_inclusive(a, b) -> RangeIter`.
- `repeat(v, n) -> RepeatIter`, `empty() -> EmptyIter`,
  `once(v) -> OnceIter`.

Example (lazy chain - `iter::filter` and `iter::map` never
allocate intermediate `Vec`s):

```
let total =
  1..=100
    |> iter::filter(|n| n % 2 == 0)
    |> iter::map(|n| n * n)
    |> iter::sum::<i64>()
```

Example (eager - input `Vec`, output `Vec`):

```
let xs = [3, 1, 4, 1, 5, 9, 2, 6]
let sorted = xs |> iter::sort_by_key(|n| -n)
```

### 10.4a `std::option`

Free-function chaining surface for `Option<T>`. Mirrors F# `Option`
module. Argument order is data-last so every entry threads with `|>`.

- `map(f, opt) -> Option<U>`.
- `and_then(f, opt) -> Option<U>` - F# `Option.bind`; flat-map.
- `filter(p, opt) -> Option<T>`.
- `default(v, opt) -> T` - F# `Option.defaultValue`.
- `default_with(f, opt) -> T` - lazy default.
- `or(alt, opt) -> Option<T>` / `or_else(f, opt) -> Option<T>`.
- `iter(f, opt) -> ()` - F# `Option.iter`.
- `is_some(opt) -> bool`, `is_none(opt) -> bool`.
- `flatten(opt: Option<Option<T>>) -> Option<T>`.
- `zip(a, b) -> Option<(A, B)>`.

The same surface is mirrored as methods on `Option<T>` for the
Rust-style call form (`opt.map(f)`, `opt.unwrap_or(0)`). Pick the
form that fits the surrounding code; don't mix in one chain.

### 10.4b `std::result`

Free-function chaining surface for `Result<T, E>`. Mirrors F#
`Result` module. Data-last.

- `map(f, r) -> Result<U, E>`.
- `map_err(f, r) -> Result<T, F>`.
- `and_then(f, r) -> Result<U, E>` - F# `Result.bind`.
- `or_else(f, r) -> Result<T, F>`.
- `default(v, r) -> T`, `default_with(f, r) -> T`.
- `ok(r) -> Option<T>`, `err(r) -> Option<E>`.
- `is_ok(r) -> bool`, `is_err(r) -> bool`.

The `?` operator (§4.5) remains the right tool for short-circuit
propagation; `result::map` / `result::and_then` are for in-pipeline
transformation when the chain doesn't return from the enclosing fn.

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

- Dynamic surface: `json::parse(text) -> Result<json::Value, Error>`,
  `json::render(value) -> String`, plus the `json::{get, at, len,
  is_null, as_str, as_i64, as_f64, as_bool, as_array, keys}` query
  helpers.
- Strict, typed surface: every named struct in the program
  auto-derives a pair of generic serializer free functions, invoked
  with a turbofish type argument (there are no `Type::from_json`
  methods):
  - `from_json::<Type>(text: &String) -> Result<Type, errors::Error>`
  - `to_json::<Type>(value: Type) -> Result<String, errors::Error>`.
  `from_json` is the canonical one-line, serde-style deserializer:
  it validates each field against the declared field type
  recursively (nested structs by source name, `[T]` / `Vec<T>` /
  `[T; N]` / tuples / `Option<T>` / `HashMap<String, V>` walk
  through, `json::Value` fields pass through). Missing required
  fields and type mismatches surface as
  `Result::Err(errors::Error)` with a path-qualified message.
- Serialization is automatic (every struct gets `to_json::<T>` /
  `from_json::<T>` using the source field names verbatim);
  `#[derive(Serialize, Deserialize)]` is rejected (`GT0025`).

### 10.12 `std::thread`, `std::channel`

- `thread::spawn(|| { ... })` - OS thread (rarely used; prefer `go`).
- `channel<T>()`, `channel<T>(cap)`.

### 10.13 `std::panic`

- `panic!(msg: String)`.
- `catch_unwind(f: impl FnOnce() -> T) -> Result<T, PanicPayload>`.

---

## 11. Build and runtime

### 11.1 Targets

The table below is the supported OS x Arch matrix. CI exercises
`linux-x86_64`, `linux-aarch64`, `darwin-x86_64`, `darwin-aarch64`, and
`windows-x86_64`. The `linux-riscv64`, `freebsd-x86_64`, and
`wasm32-wasi` rows are registered in the target table as groundwork but
do not yet produce shippable native binaries.

| OS × Arch | Backend |
|---|---|
| linux-x86_64 | Cranelift + LLVM |
| linux-aarch64 | Cranelift + LLVM |
| linux-riscv64 | LLVM (planned, not yet shipped) |
| darwin-x86_64 | Cranelift + LLVM |
| darwin-aarch64 | Cranelift + LLVM |
| windows-x86_64 | Cranelift + LLVM |
| freebsd-x86_64 | LLVM (planned, not yet shipped) |
| wasm32-wasi | planned, not yet shipped |

### 11.2 Linking

Static linking by default. The produced binary embeds the runtime. On
Linux, `musl` target produces a zero-libc static binary identical in
deployment experience to `CGO_ENABLED=0` Go.

Dynamic linking for FFI is **not** available through a source-level
syntax. See §12 for the supported FFI mechanism (`[rust-bindings]`).

On Linux, `gos build --release` produces a fully-static musl binary by
default when the host-architecture musl rustup target
(`x86_64-unknown-linux-musl` or `aarch64-unknown-linux-musl`) is
installed; pass `--dynamic` to force the dynamic-glibc link path. The
`--target` flag selects a cross-compilation triple (see §11.4).

On a Raspberry Pi (any `aarch64` Linux), `gos run` is fully
self-contained - the bytecode VM and its in-process Cranelift JIT need
no external tools. `gos build` shells out to the device's system LLVM
(`llc`/`opt`) and a C compiler (`cc`) for codegen and linking, so those
must be installed to compile natively on the Pi.

### 11.3 Compile modes

| Mode | Command | Backend | Pipeline | Speed | Output quality |
|---|---|---|---|---|---|
| Interpret | `gos run file.gos` | Bytecode VM | Direct dispatch; in-process Cranelift JIT tiers up hot bodies | Fastest cold start | No native codegen |
| Debug build | `gos build` | LLVM | `llc -O0` (no `opt` pre-pass) | Sub-second for small programs | ~2x slower than release |
| Release build | `gos build --release` | LLVM | `opt -O3 \| llc -O3 -mcpu=native -mattr=+prefer-256-bit` | Seconds for thousands of LoC | Vectorised, inlined |

LLVM is the canonical native backend; the Cranelift code path is
reserved for the in-process JIT inside `gossamer-interp` and is not
reachable from `gos build`. Any MIR shape the LLVM lowerer cannot
handle is a hard `gos build` failure rather than a silent per-function
Cranelift fallback. The register-based bytecode VM is the sole `gos
run` / `gos test` engine and lowers every construct natively; there is
no tree-walker interpreter. VM correctness is pinned by the tier-parity
suite and the VM-vs-LLVM-AOT differential.

### 11.4 Cross-compilation

```
gos build --target aarch64-unknown-linux-gnu  --release app.gos
gos build --target aarch64-unknown-linux-musl --release app.gos
```

All targets share the same frontend and MIR; only the backend pass and
the link differ. `gos build --target <triple>` produces a real native
binary when:

1. `<triple>` is a registered target;
2. a runtime archive for the target resolves - shipped in the
   toolchain's `lib/<triple>/`, set via
   `GOS_RUNTIME_LIB_<TRIPLE>`, or built with `cargo build --release
   --target <triple> -p gossamer-runtime` (no fallback to the host
   archive - a missing target archive is a hard error, never a
   foreign-architecture mislink); and
3. a linker for the target is available - the conventional
   `aarch64-linux-gnu-gcc` for a same-OS Linux cross, or `ld.lld` /
   `rust-lld` for an OS-crossing link (overridable via
   `CARGO_TARGET_<TRIPLE>_LINKER` / `GOS_CROSS_CC`).

The validated matrix is **Linux, macOS, and Windows hosts ->
`{x86_64,aarch64}-unknown-linux-{gnu,musl}`** - both Linux
architectures, glibc or musl. The musl-static target is the
host-agnostic path: rustup ships its self-contained CRT on every host
and `ld.lld` emits ELF anywhere. The
gnu-dynamic target links out of the box on a Linux host; on macOS or
Windows it additionally needs an aarch64 glibc sysroot via
`GOS_CROSS_SYSROOT`. Cross output is checked bit-identical to the
bytecode VM under QEMU in CI across all three host OSes.

Cross-compiling *to* macOS or Windows as a target is not yet supported;
it requires external SDKs (osxcross + the Apple SDK, mingw-w64) whose
licensing and availability sit outside the toolchain.

---

## 12. FFI

The source-level `extern "C" { ... }` and `#[no_mangle] extern "C" fn`
item forms are **rejected at parse time** with diagnostic code
`GP0016`. Gossamer has exactly one FFI surface: the `[rust-bindings]`
section of `project.toml`, consumed by the `gossamer-binding` crate.
The `extern` keyword remains reserved.

Foreign code is brought into a Gossamer project by declaring a Rust
crate under `[rust-bindings]` in `project.toml`. The crate registers
its entry points with `gossamer_binding::register_module!`; the
toolchain compiles the crate into a per-project runner and links it
into the produced binary or interpreter. Types crossing the boundary
use the `gossamer-binding` ABI (`Unit`, `Bool`, `I64`, `F64`, `Char`,
`String`, `Tuple`, `Vec`, `Option`, `Result`, `Opaque`, `Any`,
`Bytes`, `Map`, `Variant`, `Callback`).

```toml
# project.toml
[rust-bindings]
my-libc-wrapper = { path = "./vendor/my-libc-wrapper", version = "0.1" }
```

```rust
// vendor/my-libc-wrapper/src/lib.rs
use gossamer_binding::register_module;
register_module!("libc", {
    fn malloc(size: u64) -> u64 { /* ... */ }
    fn free(ptr: u64) -> () { /* ... */ }
});
```

```gossamer
// program.gos
use libc::{malloc, free}
fn main() {
    let p = malloc(1024)
    free(p)
}
```

FFI rules:

- Every type that crosses the boundary uses one of the
  `gossamer-binding` ABI variants. Raw `*mut T` / `*const T`
  Gossamer pointers are not part of the boundary; integer handles or
  `Opaque<T>` carry pointers when needed.
- A binding may panic; the binding ABI wraps each entry in
  `std::panic::catch_unwind` and converts panics to a returned
  `Result::Err`.
- Calls into the runner enter a scheduler state that releases the
  P, so long-running native calls do not block other goroutines.

See `docs_src/libraries.md` and `crates/gossamer-binding/ABI_0_4.md`
for the binding ABI's full surface. A source-level `extern "C"` item
form is not implemented and remains out of scope.

---

## 13. Attributes

```
#[derive(Debug, Default, PartialEq)]
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

Gossamer has a small fixed macro set, expanded at parse time; there is
no runtime macro engine and no user-defined macros. Six are
format-shaped (below); plus the desugar macros `matches!(e, pat)`,
`todo!` / `unimplemented!` / `unreachable!`, and `dbg!(e)`, and the
build-time `regex!` / `sql!` / `codegen!`.

| Macro | Returns | Destination |
|---|---|---|
| `format!("…", …)` | `String` | - |
| `println!("…", …)` | `()` | stdout + newline |
| `print!("…", …)` | `()` | stdout, no newline |
| `eprintln!("…", …)` | `()` | stderr + newline |
| `eprint!("…", …)` | `()` | stderr, no newline |
| `panic!("…", …)` | `!` | unwinds with the rendered message |

Beyond the six format macros and the desugar / build-time macros
listed above, **every other `name!(...)` is a parse error** (`GP0001`),
with a diagnostic steering the user to the plain-function form. This
includes the Rust macros a newcomer reaches for: there is no `vec!`,
`map!`, `set!`, `write!`, `writeln!`, `assert!`, `assert_eq!`,
`debug_assert!`, `include_str!`, `include_bytes!`, or `env!`.

- Collection literals use the array form `[...]` / `[v; n]`, which
  coerces to `Vec<T>` (§3.3) - there is no `vec!`.
- `assert(cond[, msg])` and `assert_eq(a, b[, msg])` are prelude
  *functions* called without a `!`; `std::testing` provides the
  non-panicking `check*` variants.

User-defined macros do not exist. Compile-time metaprogramming is
instead **Zig-style `comptime`** - ordinary Gossamer functions and
blocks evaluated at compile time, rather than a `macro_rules!`-style
macro language, with no separate macro grammar, no hygiene model, and no
token-tree DSL.

A `comptime { ... }` block, every call to a `comptime fn`, and every
argument to a `comptime` parameter are evaluated on the bytecode VM
during compilation and folded to a literal, so the bytecode VM, the
Cranelift JIT, and the LLVM AOT backend all compile the identical
constant. A region must read only compile-time-known values (literals,
consts, other `comptime fn` results) and evaluate to a scalar or string;
otherwise it is a compile error. `typeInfo::<T>()` reflects a struct's
fields (`[(name, type)]`) at compile time so a `comptime fn` can
generate per-type code, and the `regex!` / `sql!` macros validate their
argument at build time, failing the build on malformed input. See the
[`comptime` language page](docs_src/language/comptime.md).

The toolchain implements the six format-shaped macros (`println!`,
`print!`, `eprintln!`, `eprint!`, `format!`, `panic!`), the desugar
macros (`matches!`, `todo!`, `unimplemented!`, `unreachable!`, `dbg!`),
and the build-time `regex!` / `sql!` / `codegen!`. Every other
`name!(...)` form - including `vec!`, `write!`, `writeln!`, `map!`,
`set!`, `assert!`, `assert_eq!`, `debug_assert!`, `include_str!`,
`include_bytes!`, and `env!` - is rejected at parse time (`GP0001`).
User-defined macros are out of scope.

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

- **Registry** - HTTP endpoint serving signed tarballs, matched by DNS
  prefix via `[registries]`.
- **Git** - clone and check out a tag, branch, or rev.
- **Local path** - for side-by-side development of related projects.
- **URL tarball** - plain archive with mandatory sha256.

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

- `gos init` - create `project.toml` in the current directory.
- `gos new NAME` - scaffold a new project directory.
- `gos add ID[@VERSION]` - add a dependency entry.
- `gos remove ID` - remove a dependency entry.
- `gos build` - compile the current project.
- `gos run` - interpret the current project.
- `gos test` - run the project's tests.
- `gos fetch` - resolve and download (but do not build) all deps.
- `gos update` - update deps within their declared ranges.
- `gos tidy` - rewrite the manifest to its minimal closure.
- `gos vendor` - copy deps into `./vendor/`.
- `gos doc` - generate HTML documentation.

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

## Appendix A - Differences from Go

1. No `nil`. All absence goes through `Option`.
2. No implicit zero-value for function types.
3. No interfaces in Go's sense - traits with explicit `impl`.
4. No `iota` - use `const` or an enum with explicit discriminants.
5. No type switch `x.(type)` - use `match` on an enum or `match` on
   a trait object with `Any::type_id`.
6. No labeled `goto`.
7. No implicit newline-as-semicolon rule.
8. Visibility by `pub`, not by capitalization.
9. Generics syntax is `<T>`, not `[T]`.

## Appendix B - Differences from Rust

1. No lifetimes (automatic memory management removes the need).
2. No borrow checker (automatic memory management removes the need).
3. No `Drop` trait with deterministic destruction - use `defer` for
   cleanup tied to scope; the runtime reclaims memory.
4. No `Box<T>` / `Rc<T>` / `Arc<T>` - plain references are
   runtime-managed and safe to share across goroutines.
5. `&T` is a managed reference, not a borrow. `&T` and `&mut T` have the
   same runtime; the distinction is a type-check-only aliasing hint.
6. No `async`/`await` - goroutines replace the entire async story.
7. No macros in v1 (beyond built-ins).
8. `go` and `select` keywords added.
9. `defer` keyword added.
10. **Forward pipe `|>`** (F#-style, left-associative, appends the
    piped value as the **last** argument). See §4.6. Rust has no pipe
    operator; Gossamer adds it as a first-class part of the grammar.
11. **F#-style free-function combinator surface in stdlib.**
    `std::iter`, `std::option`, and `std::result` ship F#-style
    free-function chaining APIs (see §10.4-§10.4b) with data-last
    argument order so they thread cleanly through `|>`. Unlike
    Rust, collections (`Vec<T>`, `HashMap<K, V>`, `HashSet<T>`,
    `BTreeMap<K, V>`) do **not** carry `.map`/`.filter`/`.fold`/
    `.reduce`/`.partition`/etc. methods - `iter::*` free functions
    are the one obvious way to chain transformations. `Option<T>`
    and `Result<T, E>` keep their Rust-style methods
    (`opt.map(f)`, `opt.unwrap_or(0)`) alongside the free
    functions because the method form fits how those types are
    commonly used inline.

## Appendix C - Go features not ported

- `iota` (use `enum` discriminants).
- Embedded structs with method promotion (use explicit delegation or
  traits with default methods).
- `panic`/`recover` at function level (use `catch_unwind` at closure
  level).
- Init functions with ordering by import - replaced by explicit
  `fn init()` called in dependency-topological order.
- Untyped constants with arbitrary precision - literal constants have
  a default type and are coerced at use sites; infinite-precision
  compile-time arithmetic is not performed beyond what LLVM/Cranelift
  offer.
- `goto` - omitted.

---

*End of Gossamer specification (pre-1.0.0 draft).*
