# `lang::struct`

Status: shipped

Product type declaration.
<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## Functional record update

A struct literal may spread a base value with `..base` and override
individual fields:

```gossamer
let p2 = Point { ..p1, x: 10 }   // x overridden, the rest copied from p1
let p3 = Point { x: 10, ..p1 }   // the spread may appear in any position
```

- Explicit fields win over the base for the same name.
- Exactly one `..base` spread is allowed (a second is a parse error).
- Fields copied from the base share its heap children and are retained,
  so the base stays usable after the update with no double-free. Output
  is identical across the VM, Cranelift, and LLVM tiers.

## Tuple structs

A struct may carry positional fields instead of named ones:

```gossamer
struct Pt(i64, i64)

let p = Pt(3, 4)            // construction
println!("{} {}", p.0, p.1) // positional access
let Pt(a, b) = p            // destructuring (also in `match` and fn params)
```

Tuple fields are modelled as named fields `"0".."N-1"`, so construction,
`.N` access, destructuring, `#[derive(...)]`, and serde all work the same
as on a named struct. `to_json` / `from_json` use a position-keyed object
(`{"0": 3, "1": 4}`).

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
