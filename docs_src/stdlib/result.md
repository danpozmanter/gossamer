# `std::result`

Status: shipped

Data-last Result combinators for pipeline chaining: map, map_err, default_with, etc.

## Public items

| Name | Kind | Description |
|---|---|---|
| `and_then` | fn | Chains a fallible step on the Ok payload. |
| `default` | fn | Unwraps Ok with a fallback value for Err. |
| `default_with` | fn | Consumes the result, handling Err with a callback. |
| `err` | fn | Err payload as an Option. |
| `is_err` | fn | True for Err. |
| `is_ok` | fn | True for Ok. |
| `map` | fn | Transforms the Ok payload, Err passes through. |
| `map_err` | fn | Transforms the Err payload, Ok passes through. |
| `ok` | fn | Ok payload as an Option. |
| `or_else` | fn | Recovers from Err with a fallback computation. |

