# `lang::struct`

Status: shipped

Product type declaration.
<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## Functional record update

A struct literal may spread a base value with `..base` and override
individual fields:

```gossamer
let p2 = Point(10, p1.y)
```

- Explicit fields win over the base for the same name.
- Exactly one `..base` spread is allowed (a second is a parse error).
- Fields copied from the base share its heap children and are retained,
  so the base stays usable after the update with no double-free. Output
  is identical across the VM, Cranelift, and LLVM tiers.

## Declaration and construction

Struct declarations always use named fields in braces. Construction uses
parentheses, with arguments assigned in declaration order:

```gossamer
struct Pt { x: i64, y: i64 }

let p = Pt(3, 4)
println!("{} {}", p.x, p.y)
let Pt { x, y } = p
```

Use `struct Marker {}` and `Marker()` for an empty struct. Tuple declarations
such as `struct Pt(i64, i64)` and bare unit declarations such as `struct
Marker` are rejected.

## Value semantics: copy and compare with no derive

Structs are value types. Binding or `.clone()`-ing copies the whole value
(heap children retained), and `==` / `!=` / `<` / `<=` / `>` / `>=` are
synthesized automatically - no `#[derive]` needed - whenever every field
is comparable (scalars, `String`, nested comparable types). Ordering is
lexicographic by field declaration order:

```gossamer
struct Point { x: i64, y: i64 }

let a = Point(1, 2)
let b = a.clone()                          // `clone` is a universal builtin
println!("{}", a == b)                     // true, no derive
println!("{}", a < Point(1, 3))   // true (lexicographic by field)
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
