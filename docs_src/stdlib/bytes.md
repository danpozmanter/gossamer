# `std::bytes`

Status: experimental

Byte buffers, builders, and slice helpers.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Buffer` | type | Growable byte buffer. |
| `Builder` | type | Incremental string builder. |
| `index_of` | fn | First occurrence of a byte needle. |
| `split` | fn | Splits on every separator occurrence. |
| `replace` | fn | Replaces every occurrence of a byte needle. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/bytes.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Buffer`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/bytes.rs) | `type Buffer` | Growable byte buffer. |
| [`Builder`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/bytes.rs) | `type Builder` | Incremental string builder. |
| [`index_of`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/bytes.rs) | `fn index_of(haystack: Vec<u8>, needle: Vec<u8>) -> Option<i64>` | First occurrence of a byte needle. |
| [`replace`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/bytes.rs) | `fn replace(haystack: Vec<u8>, from: Vec<u8>, to: Vec<u8>) -> Vec<u8>` | Replaces every occurrence of a byte needle. |
| [`split`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/bytes.rs) | `fn split(haystack: Vec<u8>, sep: Vec<u8>) -> Vec<Vec<u8>>` | Splits on every separator occurrence. |
