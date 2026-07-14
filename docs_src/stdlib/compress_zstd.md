# `std::compress::zstd`

Status: shipped

Zstandard encoder / decoder (RFC 8478; libzstd-vendored).

## Public items

| Name | Kind | Description |
|---|---|---|
| `encode` | fn | One-shot Zstandard compress at the default level (3). |
| `encode_level` | fn | One-shot Zstandard compress at the supplied level (1 fastest -- 22 best). |
| `decode` | fn | One-shot Zstandard decompress. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/compress/zstd.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`decode`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/compress/zstd.rs) | `fn decode(data: Vec<u8>) -> Result<Vec<u8>, errors::Error>` | One-shot Zstandard decompress. |
| [`encode`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/compress/zstd.rs) | `fn encode(data: Vec<u8>) -> Result<Vec<u8>, errors::Error>` | One-shot Zstandard compress at the default level (3). |
| [`encode_level`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/compress/zstd.rs) | `fn encode_level(data: Vec<u8>, level: i64) -> Result<Vec<u8>, errors::Error>` | One-shot Zstandard compress at the supplied level (1 fastest -- 22 best). |
