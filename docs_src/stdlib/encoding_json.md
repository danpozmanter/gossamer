# `std::encoding::json`

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

