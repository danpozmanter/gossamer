# `std::encoding::json`

Status: shipped

JSON parser, emitter, and derive support.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Serialize` | trait | Trait for converting a value to JSON. |
| `Deserialize` | trait | Trait for parsing a value from JSON. |
| `encode` | fn | Encodes a `Serialize` value as a JSON `String`. |
| `decode` | fn | Decodes a JSON `String` into a `Deserialize` value. |
| `Value` | type | Dynamically typed JSON value. |
| `Error` | type | Error raised by encoding/decoding operations. |
| `parse` | fn | Parses JSON text into a dynamic Value. |
| `render` | fn | Renders a dynamic Value as compact JSON text. |
| `encode_pretty` | fn | Renders a value as indented JSON text. |
| `valid` | fn | Reports whether the text is well-formed JSON. |
| `get` | fn | Looks up an object field on a dynamic Value. |
| `set` | fn | Sets an object field on a dynamic Value. |
| `at` | fn | Indexes an array element on a dynamic Value. |
| `keys` | fn | Object field names of a dynamic Value. |
| `len` | fn | Element / field count of a dynamic Value. |
| `is_null` | fn | Reports whether a dynamic Value is null. |
| `as_str` | fn | Reads a dynamic Value as Option<String>. |
| `as_i64` | fn | Reads a dynamic Value as Option<i64>. |
| `as_f64` | fn | Reads a dynamic Value as Option<f64>. |
| `as_bool` | fn | Reads a dynamic Value as Option<bool>. |
| `as_array` | fn | Reads a dynamic Value as an array of Values. |

