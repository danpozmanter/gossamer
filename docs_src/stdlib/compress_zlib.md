# `std::compress::zlib`

Status: shipped

zlib (RFC 1950) encoder / decoder.

## Public items

| Name | Kind | Description |
|---|---|---|
| `compress` | fn | One-shot zlib compress. |
| `decompress` | fn | One-shot zlib decompress. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/compress/zlib.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`compress`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/compress/zlib.rs) | `fn compress(data: Vec<u8>, level: i64) -> Result<Vec<u8>, errors::Error>` | One-shot zlib compress. |
| [`decompress`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/compress/zlib.rs) | `fn decompress(data: Vec<u8>) -> Result<Vec<u8>, errors::Error>` | One-shot zlib decompress. |
