# `std::encoding::binary`

Status: shipped

Big/little-endian integer packing and varint codecs.

## Public items

| Name | Kind | Description |
|---|---|---|
| `get_u8` | fn | Reads a single byte. |
| `put_u8` | fn | Writes a single byte. |
| `get_u16_be` | fn | Reads a big-endian u16. |
| `put_u16_be` | fn | Writes a big-endian u16. |
| `get_u16_le` | fn | Reads a little-endian u16. |
| `put_u16_le` | fn | Writes a little-endian u16. |
| `get_u32_be` | fn | Reads a big-endian u32. |
| `put_u32_be` | fn | Writes a big-endian u32. |
| `get_u32_le` | fn | Reads a little-endian u32. |
| `put_u32_le` | fn | Writes a little-endian u32. |
| `get_u64_be` | fn | Reads a big-endian u64. |
| `put_u64_be` | fn | Writes a big-endian u64. |
| `get_u64_le` | fn | Reads a little-endian u64. |
| `put_u64_le` | fn | Writes a little-endian u64. |
| `uvarint` | fn | Decodes an unsigned varint. |
| `varint` | fn | Decodes a signed varint (zigzag). |
| `put_uvarint` | fn | Encodes an unsigned varint. |
| `put_varint` | fn | Encodes a signed varint (zigzag). |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`get_u16_be`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn get_u16_be(bytes: Vec<u8>) -> Result<i64, errors::Error>` | Reads a big-endian u16. |
| [`get_u16_le`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn get_u16_le(bytes: Vec<u8>) -> Result<i64, errors::Error>` | Reads a little-endian u16. |
| [`get_u32_be`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn get_u32_be(bytes: Vec<u8>) -> Result<i64, errors::Error>` | Reads a big-endian u32. |
| [`get_u32_le`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn get_u32_le(bytes: Vec<u8>) -> Result<i64, errors::Error>` | Reads a little-endian u32. |
| [`get_u64_be`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn get_u64_be(bytes: Vec<u8>) -> Result<i64, errors::Error>` | Reads a big-endian u64. |
| [`get_u64_le`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn get_u64_le(bytes: Vec<u8>) -> Result<i64, errors::Error>` | Reads a little-endian u64. |
| [`get_u8`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn get_u8(bytes: Vec<u8>) -> Result<i64, errors::Error>` | Reads a single byte. |
| [`put_u16_be`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn put_u16_be(buf: Vec<u8>, value: i64) -> Vec<u8>` | Writes a big-endian u16. |
| [`put_u16_le`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn put_u16_le(buf: Vec<u8>, value: i64) -> Vec<u8>` | Writes a little-endian u16. |
| [`put_u32_be`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn put_u32_be(buf: Vec<u8>, value: i64) -> Vec<u8>` | Writes a big-endian u32. |
| [`put_u32_le`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn put_u32_le(buf: Vec<u8>, value: i64) -> Vec<u8>` | Writes a little-endian u32. |
| [`put_u64_be`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn put_u64_be(buf: Vec<u8>, value: i64) -> Vec<u8>` | Writes a big-endian u64. |
| [`put_u64_le`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn put_u64_le(buf: Vec<u8>, value: i64) -> Vec<u8>` | Writes a little-endian u64. |
| [`put_u8`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn put_u8(buf: Vec<u8>, value: i64) -> Vec<u8>` | Writes a single byte. |
| [`put_uvarint`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn put_uvarint(buf: Vec<u8>, value: i64) -> Vec<u8>` | Encodes an unsigned varint. |
| [`put_varint`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn put_varint(buf: Vec<u8>, value: i64) -> Vec<u8>` | Encodes a signed varint (zigzag). |
| [`uvarint`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn uvarint(bytes: Vec<u8>) -> Result<i64, errors::Error>` | Decodes an unsigned varint. |
| [`varint`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn varint(bytes: Vec<u8>) -> Result<i64, errors::Error>` | Decodes a signed varint (zigzag). |
