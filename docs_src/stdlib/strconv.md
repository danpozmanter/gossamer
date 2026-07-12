# `std::strconv`

Status: experimental

Conversions between strings and primitive numeric types.

## Public items

| Name | Kind | Description |
|---|---|---|
| `parse_i64` | fn | Parses a decimal `i64`. |
| `parse_u64` | fn | Parses a decimal `u64`. |
| `parse_f64` | fn | Parses a decimal `f64`. |
| `parse_bool` | fn | Parses `"true"` / `"false"` into a bool. |
| `format_i64` | fn | Renders an `i64` as a decimal string. |
| `format_f64` | fn | Renders an `f64` as a decimal string. |
| `parse_i64_radix` | fn | Parses an i64 from a string in the given base (2..=36). |
| `format_i64_radix` | fn | Formats an i64 in the given base (2..=36). |
| `quote` | fn | Wraps a string in double quotes with escapes. |
| `unquote` | fn | Removes surrounding quotes and resolves escapes. |

