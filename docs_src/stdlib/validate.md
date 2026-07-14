# `std::validate`

Status: experimental

Trait-based field validation: implement Validate, collect FieldErrors into Errors.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Validate` | trait | Implement on a struct to declare field-level validation rules. |
| `FieldError` | type | One field-scoped validation failure: dotted path, message, optional code. |
| `Errors` | type | Aggregated FieldError set, indexable by dotted path. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/validate.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Errors`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/validate.rs) | `type Errors` | Aggregated FieldError set, indexable by dotted path. |
| [`FieldError`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/validate.rs) | `type FieldError` | One field-scoped validation failure: dotted path, message, optional code. |
| [`Validate`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/validate.rs) | `trait Validate` | Implement on a struct to declare field-level validation rules. |
