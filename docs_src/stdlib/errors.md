# `std::errors`

Error construction, wrapping, and chain traversal.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Error` | type | Reference-counted error value with optional cause chain. |
| `new` | fn | Constructs a fresh error from a message. |
| `newf` | fn | Constructs a fresh error from a format template, e.g. `newf("status {}", code)`. |
| `wrap` | fn | Wraps a cause with a higher-level message. |
| `is` | fn | Checks whether an error's chain contains a matching message. |
| `chain` | fn | Iterator over an error and its ancestor causes. |
| `join` | fn | Joins a list of errors into a single piped error. |

