# `lang::match`

Status: shipped

Exhaustive pattern match expression.
<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

`match` is an expression: every arm yields a value and the whole `match`
binds to it. Arms must be exhaustive - a non-exhaustive `match` is a
compile error (`GM0001`), so add a `_` arm only when every remaining case
genuinely means the same thing.

```gossamer
let label = match shape {
    Shape::Circle(r) => "round",
    Shape::Rect { w, h } if w == h => "square",
    Shape::Rect { .. } => "boxy",
}
```

## Patterns

- Literals, `_` wildcard, and bindings (`name`, `mut name`).
- Enum variants: tuple `Shape::Line(n)`, struct `Shape::Named { id }`,
  unit `Shape::Dot`.
- Tuple-struct patterns: `Pt(a, b)` (a tuple struct destructures
  positionally, like a tuple).
- Struct patterns `Point { x, y }`, renamed `Point { x: a, y: b }`.
- Tuples `(a, b)`; rest `..` for the unmatched fields (`Shape::Box(..)`,
  `Point { x, .. }`).
- Ranges: closed `1..=5`, exclusive `1..5`, open-ended (`..=hi`, `lo..`);
  a range pattern is opaque to exhaustiveness, so a `_` arm is still
  required.
- Or-patterns `a | b`, `@`-bindings `n @ 1..=3`, and guards `Some(n) if n
  > 0`.

```gossamer
let kind = match s {
    Shape::Dot => 0,
    Shape::Line(..) => 1,
    Shape::Box(w, h) if w == h => 2,
    Shape::Box(..) => 3,
}
```

Patterns also drive `let`, `if let` / `while let`, `for`, and function
parameters - see [fn](fn.md) for parameter destructuring. The
`matches!(expr, pat)` macro is a boolean one-arm match.
