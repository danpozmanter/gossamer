# `std::encoding::json`

Status: experimental

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

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Deserialize`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `trait` — see the source declaration | Trait for parsing a value from JSON. |
| [`Error`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `type` — see the source declaration | Error raised by encoding/decoding operations. |
| [`Serialize`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `trait` — see the source declaration | Trait for converting a value to JSON. |
| [`Value`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `type` — see the source declaration | Dynamically typed JSON value. |
| [`as_array`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn as_array(value: json::Value) -> Option<Vec<json::Value>>` | Reads a dynamic Value as an array of Values. |
| [`as_bool`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn as_bool(value: json::Value) -> Option<bool>` | Reads a dynamic Value as Option<bool>. |
| [`as_f64`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn as_f64(value: json::Value) -> Option<f64>` | Reads a dynamic Value as Option<f64>. |
| [`as_i64`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn as_i64(value: json::Value) -> Option<i64>` | Reads a dynamic Value as Option<i64>. |
| [`as_str`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn as_str(value: json::Value) -> Option<String>` | Reads a dynamic Value as Option<String>. |
| [`at`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn at(value: json::Value, index: i64) -> Option<json::Value>` | Indexes an array element on a dynamic Value. |
| [`decode`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn decode(source: String) -> Result<json::Value, errors::Error>` | Decodes a JSON `String` into a `Deserialize` value. |
| [`encode`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn encode(value: json::Value) -> String` | Encodes a `Serialize` value as a JSON `String`. |
| [`encode_pretty`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn encode_pretty(value: json::Value) -> String` | Renders a value as indented JSON text. |
| [`get`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn get(value: json::Value, key: String) -> Option<json::Value>` | Looks up an object field on a dynamic Value. |
| [`is_null`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn is_null(value: json::Value) -> bool` | Reports whether a dynamic Value is null. |
| [`keys`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn keys(value: json::Value) -> Option<Vec<String>>` | Object field names of a dynamic Value. |
| [`len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn len(value: json::Value) -> i64` | Element / field count of a dynamic Value. |
| [`parse`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn parse(source: String) -> Result<json::Value, errors::Error>` | Parses JSON text into a dynamic Value. |
| [`render`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn render(value: json::Value) -> String` | Renders a dynamic Value as compact JSON text. |
| [`set`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn set(value: json::Value, key: String, next: json::Value) -> json::Value` | Sets an object field on a dynamic Value. |
| [`valid`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn valid(source: String) -> bool` | Reports whether the text is well-formed JSON. |
