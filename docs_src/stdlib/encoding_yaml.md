# `std::encoding::yaml`

Status: experimental

YAML 1.2 parser/emitter (serde_norway-backed).

## Public items

| Name | Kind | Description |
|---|---|---|
| `Value` | type | Dynamically typed YAML value. |
| `parse` | fn | Parses a YAML document into a Value. |
| `encode` | fn | Encodes a Value as a YAML document. |
| `parse_all` | fn | Parses a multi-document YAML stream into a Vec<Value>. |
| `to_json` | fn | Converts a YAML document to JSON text. |
| `from_json` | fn | Converts JSON text to a YAML document. |
| `is_valid` | fn | Reports whether the text is well-formed YAML. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/yaml.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Value`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/yaml.rs) | `type Value` | Dynamically typed YAML value. |
| [`encode`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/yaml.rs) | `fn encode(value: json::Value) -> Result<String, errors::Error>` | Encodes a Value as a YAML document. |
| [`from_json`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/yaml.rs) | `fn from_json(source: String) -> Result<String, errors::Error>` | Converts JSON text to a YAML document. |
| [`is_valid`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/yaml.rs) | `fn is_valid(source: String) -> bool` | Reports whether the text is well-formed YAML. |
| [`parse`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/yaml.rs) | `fn parse(source: String) -> Result<json::Value, errors::Error>` | Parses a YAML document into a Value. |
| [`parse_all`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/yaml.rs) | `fn parse_all(source: String) -> Result<Vec<json::Value>, errors::Error>` | Parses a multi-document YAML stream into a Vec<Value>. |
| [`to_json`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/yaml.rs) | `fn to_json(source: String) -> Result<String, errors::Error>` | Converts a YAML document to JSON text. |
