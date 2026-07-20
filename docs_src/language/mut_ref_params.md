# `lang::mut_ref_params`

Local `&mut` aliases write through; `&mut Vec<T>` / `&mut [T]` parameters write through on every tier.
<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

`&mut T` parameters write through to the caller's source place on every tier
for scalar values, strings, vectors, slices, structs, and enums. `&mut`
references are aliases, not copied argument values. Gossamer does not impose
Rust lifetime or non-lexical-borrow checking. It does reject a second simple
named `&mut` to the same root while the first remains in lexical scope, but it
does not track every alias shape. Every permitted write is observed through
the same source place.

Fixed arrays are the ABI exception: a `[T; N]` argument is copied at a
function-call boundary, so a mutable parameter observes and mutates that
call-local copy. A local reference such as `let r = &mut xs` still aliases
its fixed-array source and writes through normally.
