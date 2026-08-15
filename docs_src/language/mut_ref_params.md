# `lang::mut_ref_params`

Local `&mut` aliases write through; `&mut Vec<T>` / `&mut [T]` parameters write through on every tier.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

`&mut T` parameters write through to the caller's source place on every tier
for scalar values, strings, vectors, slices, structs, enums, and fixed arrays.
`&mut` references are exclusive lexical views, not copied argument values.
Their implicit lifetime ends at the closing brace. Gossamer has no explicit
lifetime annotations and does not shorten a borrow at its last use. It rejects
a second named `&mut` to the same root while the first remains in scope, an
overlapping temporary borrow, and two `&mut` arguments rooted at the same
binding in one call. Every permitted write is observed through the same source
place.

A call never creates `&mut` implicitly. Pass a mutable place explicitly:

```gossamer
fn clear(value: &mut i64) { *value = 0 }

let mut value = 1
clear(&mut value)
```

`clear(value)` is rejected even when `value` is a mutable binding. If a
parameter or local already has type `&mut T`, pass that reference directly:
`fn forward(value: &mut i64) { clear(value) }`.
