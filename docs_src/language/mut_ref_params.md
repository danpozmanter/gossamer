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

Every container obeys the same rule the rest of the language does: a `&mut T`
parameter writes through to the caller's container, and a `T` parameter takes
a value of its own, so the callee's `push` or `insert` leaves the caller's
untouched. `Map`, `Set`, `BTreeMap`, `BTreeSet`, `Vec`, `Deque`, `Queue`,
`Stack`, `MinHeap`, and `MaxHeap` all reach their elements through a handle,
so the copy is made where the binding or the argument is taken rather than
inherited from the representation - but nothing about that is visible in the
language. A binding taken from a container (`let b = a`) is a value of its own
for the same reason.

A call never creates `&mut` implicitly. Pass a mutable place explicitly:

```gossamer
fn clear(value: &mut i64) { *value = 0 }

let mut value = 1
clear(&mut value)
```

`clear(value)` is rejected even when `value` is a mutable binding. If a
parameter or local already has type `&mut T`, pass that reference directly:
`fn forward(value: &mut i64) { clear(value) }`.
