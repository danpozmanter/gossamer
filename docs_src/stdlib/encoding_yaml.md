# `std::encoding::yaml`

Status: shipped

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

