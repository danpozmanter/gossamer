# `std::encoding::xml`

Status: experimental

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

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/xml.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Event`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/xml.rs) | `type` — see the source declaration | Start / End / Text / CData / Comment / Eof. |
| [`Reader`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/xml.rs) | `type` — see the source declaration | Pull-style XML reader. |
| [`Writer`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/xml.rs) | `type` — see the source declaration | Streaming XML writer. |
| [`encode`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/xml.rs) | `fn encode(value: json::Value) -> String` | Serialises a sequence of events to XML text. |
| [`escape`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/xml.rs) | `fn escape(text: String) -> String` | Escapes XML metacharacters in text. |
| [`parse`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/xml.rs) | `fn parse(source: String) -> Result<json::Value, errors::Error>` | Parses an XML document into a Vec of events. |
