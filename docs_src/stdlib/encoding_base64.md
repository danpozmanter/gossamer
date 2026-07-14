# `std::encoding::base64`

Status: experimental

RFC 4648 base64 encode/decode.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`decode`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn decode(text: String) -> Result<Vec<u8>, errors::Error>` | Decodes a base64 string. |
| [`encode`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn encode(data: Vec<u8>) -> String` | Encodes bytes to a base64 string. |
