# `lang::enum`

Sum type declaration with payload-carrying variants.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

Variants carry tuple payloads (`Line(i64)`, `Rect(i64, i64)`), struct
payloads (`Named { id: i64 }`), or nothing (`Dot`). Recursive payloads
work directly with no wrapper - `enum List { Cons(i64, List), Nil }` -
because every variant payload is already heap-shared. The `Box<List>` /
`Arc<List>` / `Rc<List>` spellings are transparent and compile to the
same thing, so reach for them only when the type reads clearer with the
wrapper. Match exhaustively.

## Compare by value, no derive

Enums are value types, so `==` / `!=` / `<` / `<=` / `>` / `>=` are
synthesized automatically - no `#[derive]` needed - whenever every
payload is comparable. Ordering is by **variant rank first** (declaration
order), then payload lexicographically:

```gossamer
enum Shape { Dot, Line(i64), Box(i64, i64) }

println!("{}", Shape::Dot < Shape::Line(0))     // true: Dot ranks before Line
println!("{}", Shape::Line(1) < Shape::Line(2)) // true: same rank, payload compared
println!("{}", Shape::Box(1, 2) == Shape::Box(1, 2))
```

A user `impl` of `eq` / `cmp` overrides the synthesized one for custom
ordering.

## Derivable traits

`#[derive(...)]` covers `Debug`, `Default`, `PartialEq`, `Eq`,
`PartialOrd`, and `Ord` for tuple, unit, and struct-payload variants;
`#[default]` marks the `Default` variant. The derive only *forces*
synthesis where the automatic gate is conservative (generic or
container-typed payloads).

```gossamer
#[derive(Debug, Default, PartialEq)]
enum Move {
    #[default]
    Stay,
    Step(i64),
}
```

`Clone`, `Copy`, `Hash`, `Display`, `Serialize`, and `Deserialize` are
**not** derivable (`GT0025`) - copying, hashing, and serialization are
automatic. The enum cap is 256 variants (`GT0012`).
