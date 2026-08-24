# `std::encoding::json`

Status: experimental

JSON parser, emitter, and derive support.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Deserialize`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `trait Deserialize` | Trait for parsing a value from JSON. |
| [`Error`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `type Error` | Error raised by encoding/decoding operations. |
| [`Serialize`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `trait Serialize` | Trait for converting a value to JSON. |
| [`Value`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `type Value` | Dynamically typed JSON value. |
| [`as_array`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn as_array(value: json::Value) -> Option<Vec<json::Value>>` | Reads a dynamic Value as an array of Values. |
| [`as_bool`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn as_bool(value: json::Value) -> Option<bool>` | Reads a dynamic Value as Option<bool>. |
| [`as_f64`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn as_f64(value: json::Value) -> Option<f64>` | Reads a dynamic Value as Option<f64>. |
| [`as_i64`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn as_i64(value: json::Value) -> Option<i64>` | Reads a dynamic Value as Option<i64>. |
| [`as_str`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn as_str(value: json::Value) -> Option<String>` | Reads a dynamic Value as Option<String>. |
| [`at`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn at(value: json::Value, index: i64) -> Option<json::Value>` | Indexes an array element on a dynamic Value. |
| [`decode`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn decode(source: String) -> Result<json::Value, errors::Error>` | Decodes a JSON `String` into a `Deserialize` value. |
| [`encode`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn encode(value: json::Value) -> String` | Encodes a `Serialize` value as a JSON `String`. |
| [`encode_pretty`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn encode_pretty(value: json::Value) -> String` | Renders a value as indented JSON text. |
| [`get`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn get(value: json::Value, key: String) -> Option<json::Value>` | Looks up an object field on a dynamic Value. |
| [`is_null`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn is_null(value: json::Value) -> bool` | Reports whether a dynamic Value is null. |
| [`keys`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn keys(value: json::Value) -> Option<Vec<String>>` | Object field names of a dynamic Value. |
| [`len`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn len(value: json::Value) -> i64` | Element / field count of a dynamic Value. |
| [`parse`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn parse(source: String) -> Result<json::Value, errors::Error>` | Parses JSON text into a dynamic Value. |
| [`render`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn render(value: json::Value) -> String` | Renders a dynamic Value as compact JSON text. |
| [`set`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn set(value: json::Value, key: String, next: json::Value) -> json::Value` | Sets an object field on a dynamic Value. |
| [`valid`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn valid(source: String) -> bool` | Reports whether the text is well-formed JSON. |
