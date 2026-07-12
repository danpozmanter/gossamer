# `std::encoding::xml`

Status: shipped

Streaming XML decoder + builder (quick-xml).

## Public items

| Name | Kind | Description |
|---|---|---|
| `Reader` | type | Pull-style XML reader. |
| `Writer` | type | Streaming XML writer. |
| `Event` | type | Start / End / Text / CData / Comment / Eof. |
| `parse` | fn | Parses an XML document into a Vec of events. |
| `encode` | fn | Serialises a sequence of events to XML text. |
| `escape` | fn | Escapes XML metacharacters in text. |

