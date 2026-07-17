# `lang::let_mut`

Status: shipped

A mutable binding can be reassigned and can be the source of `&mut`.
The two permissions are distinct from a reference's own mutability:

```gossamer
let mut value = [1, 2]
let reference = &mut value
reference[0] = 0       // writes value

let mut shared = &value
shared = &[3, 4]       // rebinds shared; it remains read-only
```

An immutable binding cannot be used as the source of `&mut`, because
that would create a write path around its immutability.
