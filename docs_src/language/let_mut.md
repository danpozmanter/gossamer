# `lang::let_mut`

Mutable bindings can be reassigned and can be the source of `&mut`.
<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

Binding mutability and reference capability are separate:

```gossamer
let mut value = [1, 2]
let reference = &mut value
reference[0] = 0       // writes value

let mut shared = &value
shared = &[3, 4]       // rebinds shared; it remains read-only
```

An immutable binding cannot be used as the source of `&mut`, because that
would create a write path around its immutability.

Implicit dereferencing does not weaken this rule. If an access path contains
any shared `&T` layer, indexing, field assignment, and mutating method calls
through that path are rejected even when every outer reference is `&mut`.
