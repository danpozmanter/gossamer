# `std::compress::gzip`

Status: experimental

gzip encoder / decoder (RFC 1952; flate2-backed).

## Public items

| Name | Kind | Description |
|---|---|---|
| `Level` | type | Compression level (`0` store-only … `9` best); default is gzip(1)'s `6`. |
| `encode` | fn | Compresses bytes at the supplied Level. |
| `decode` | fn | Decompresses a gzip-formatted payload. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/compress/gzip.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Level`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/compress/gzip.rs) | `type` — see the source declaration | Compression level (`0` store-only … `9` best); default is gzip(1)'s `6`. |
| [`decode`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/compress/gzip.rs) | `fn decode(data: Vec<u8>) -> Result<Vec<u8>, errors::Error>` | Decompresses a gzip-formatted payload. |
| [`encode`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/compress/gzip.rs) | `fn encode(data: Vec<u8>, level: i64) -> Result<Vec<u8>, errors::Error>` | Compresses bytes at the supplied Level. |
