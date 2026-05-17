# `std::strconv`

Status: shipped

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
| `parse_int` | fn | Alias for parse_i64. |
| `atoi` | fn | Alias for parse_i64 (Go-style spelling). |
| `parse_float` | fn | Alias for parse_f64. |
| `format_int` | fn | Alias for format_i64. |
| `itoa` | fn | Alias for format_i64 (Go-style spelling). |
| `format_float` | fn | Alias for format_f64. |

