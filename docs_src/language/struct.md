# `lang::struct`

Status: shipped

Product type declaration.
<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## Functional record update

A struct literal may spread a base value with `..base` and override
individual fields:

```gossamer
let p2 = Point { x: 10, y: p1.y }
```

- Explicit fields win over the base for the same name.
- Exactly one `..base` spread is allowed (a second is a parse error).
- Fields copied from the base share its heap children and are retained,
  so the base stays usable after the update with no double-free. Output
  is identical across the VM, Cranelift, and LLVM tiers.

## Declaration and construction

Named structs use braced declarations and braced construction. Tuple
structs use tuple declarations and parenthesized construction:

```gossamer
struct Pt { x: i64, y: i64 }
struct Pair(String, i64)

let p = Pt { x: 3, y: 4 }     // keyed fields, any order
let q = Pt { 3, 4 }           // positional values, declaration order
let r = Pt { y: 4, 3 }        // mixed; positional fills the next unfilled field
let pair = Pair("row", 4)
println!("{} {}", p.x, p.y)
println!("{} {}", pair.0, pair.1)
let Pt { x, y } = p
```

Named structs must be constructed with `Name { ... }`; `Name(...)` is
rejected for named structs. Inside the braces, keyed fields may appear in any
order and positional values fill declaration-order fields that were not already
filled by keyed entries. Tuple structs must be constructed with `Name(...)`;
`Name { ... }` is rejected for tuple structs.

## Value semantics: copy and compare with no derive

Structs are value types. Binding or `.clone()`-ing copies the whole value
(heap children retained), and `==` / `!=` / `<` / `<=` / `>` / `>=` are
synthesized automatically - no `#[derive]` needed - whenever every field
is comparable (scalars, `String`, nested comparable types). Ordering is
lexicographic by field declaration order:

```gossamer
struct Point { x: i64, y: i64 }

let a = Point { x: 1, y: 2 }
let b = a.clone()                          // `clone` is a universal builtin
println!("{}", a == b)                     // true, no derive
println!("{}", a < Point { x: 1, y: 3 })   // true (lexicographic by field)
```

A user `impl` of `eq` / `cmp` overrides the synthesized comparison.

## Derivable traits

`#[derive(...)]` is limited to `Debug`, `Default`, `PartialEq`, `Eq`,
`PartialOrd`, and `Ord`, synthesized as real source so `{:?}`,
`Type::default()`, and the comparisons work on every tier. The derive
only *forces* synthesis where the automatic gate is conservative (generic
or container-typed fields):

```gossamer
#[derive(Debug, Default, PartialEq)]
struct Config { retries: i64, verbose: bool }
```

`Clone`, `Copy`, `Hash`, `Display`, `Serialize`, and `Deserialize` are
**not** derivable (`GT0025`): copying, hashing, and serialization are
already automatic. Conversion / operator traits (`From`, `Add`, ...) are
written `impl Trait for T`, not derived.
