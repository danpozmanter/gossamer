# `lang::attribute`

Built-in attributes (`#[cfg]`, `#[test]`, `#[bench]`, `#[derive]`).

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

The attribute set is fixed - there are no user-defined attributes.

| Attribute | Applies to | Effect |
|---|---|---|
| `#[test]` | `fn` | Run by `gos test`. |
| `#[bench]` | `fn` | Timed by `gos bench`. |
| `#[cfg(...)]` | item | Conditional compilation (see [cfg](cfg.md)). |
| `#[cfg(test)]` | `mod` | Test-only module (give each a unique name in a project). |
| `#[derive(...)]` | `struct` / `enum` | Synthesize the listed traits. |
| `#[default]` | enum variant | Marks the `Default` variant. |

## `#[derive(...)]`

The derivable set is exactly `Debug`, `Default`, `PartialEq`, `Eq`,
`PartialOrd`, and `Ord`, synthesized as real source so `{:?}`,
`Type::default()`, and the comparison operators work on every tier:

```gossamer
#[derive(Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Version { major: i64, minor: i64 }
```

Comparison and `Default` are *automatic* for ordinary value types; the
derive only forces synthesis where the automatic gate is conservative
(generic or container-typed fields). `Clone`, `Copy`, `Hash`, `Display`,
`Serialize`, and `Deserialize` are **rejected** (`GT0025`) - copying
(`let b = a`, `a.clone()`), hashing, comparison, and serialization are
already automatic, and conversion / operator traits are written
`impl Trait for T`.
