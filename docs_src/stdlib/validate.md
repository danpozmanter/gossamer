# `std::validate`

Status: shipped

Trait-based field validation: implement Validate, collect FieldErrors into Errors.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Validate` | trait | Implement on a struct to declare field-level validation rules. |
| `FieldError` | type | One field-scoped validation failure: dotted path, message, optional code. |
| `Errors` | type | Aggregated FieldError set, indexable by dotted path. |

