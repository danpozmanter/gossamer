# `std::option`

Status: shipped

Data-last Option combinators for pipeline chaining: map, filter, unwrap_or, and_then, etc.

## Public items

| Name | Kind | Description |
|---|---|---|
| `and_then` | fn | Chains a fallible step: Some(v) -> f(v), None stays None. |
| `unwrap_or` | fn | Unwraps with a fallback value for None. |
| `unwrap_or_else` | fn | Unwraps with a lazily computed fallback for None. |
| `filter` | fn | Keeps Some(v) only when the predicate holds. |
| `flatten` | fn | Collapses Option<Option<T>> one level. |
| `is_none` | fn | True for None. |
| `is_some` | fn | True for Some. |
| `iter` | fn | Zero-or-one element sequence view. |
| `map` | fn | Transforms the Some payload, None stays None. |
| `or` | fn | First Some of self and the alternative. |
| `or_else` | fn | First Some of self and a lazily built alternative. |
| `zip` | fn | Pairs two Somes into Some((a, b)). |

