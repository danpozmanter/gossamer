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
