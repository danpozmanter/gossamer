# `std::encoding::base32`

Status: experimental

RFC 4648 base32 (uppercase) encode / decode.

## Public items

| Name | Kind | Description |
|---|---|---|
| `encode` | fn | Bytes -> base32 string. |
| `decode` | fn | Base32 string -> bytes. |
| `encode_string` | fn | Encodes a String as standard base32 text. |
| `decode_string` | fn | Decodes standard base32 text into a String. |
| `encode_hex` | fn | Encodes a String as extended-hex base32 text. |
| `decode_hex` | fn | Decodes extended-hex base32 text into a String. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/base32.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`decode`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/base32.rs) | `fn decode(text: String) -> Result<Vec<u8>, errors::Error>` | Base32 string -> bytes. |
| [`decode_hex`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/base32.rs) | `fn decode_hex(text: String) -> Result<Vec<u8>, errors::Error>` | Decodes extended-hex base32 text into a String. |
| [`decode_string`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/base32.rs) | `fn decode_string(text: String) -> Result<String, errors::Error>` | Decodes standard base32 text into a String. |
| [`encode`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/base32.rs) | `fn encode(data: Vec<u8>) -> String` | Bytes -> base32 string. |
| [`encode_hex`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/base32.rs) | `fn encode_hex(data: Vec<u8>) -> String` | Encodes a String as extended-hex base32 text. |
| [`encode_string`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding/base32.rs) | `fn encode_string(text: String) -> String` | Encodes a String as standard base32 text. |
