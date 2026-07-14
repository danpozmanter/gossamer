# `std::hash::fnv`

Status: shipped

FNV-1a non-cryptographic hash (32-bit, 64-bit).

## Public items

| Name | Kind | Description |
|---|---|---|
| `hash32` | fn | 32-bit FNV-1a of a byte slice. |
| `hash64` | fn | 64-bit FNV-1a of a byte slice. |
| `hash_string` | fn | 64-bit FNV-1a of a String. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/hash/fnv.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`hash32`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/hash/fnv.rs) | `fn hash32(data: Vec<u8>) -> i64` | 32-bit FNV-1a of a byte slice. |
| [`hash64`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/hash/fnv.rs) | `fn hash64(data: Vec<u8>) -> i64` | 64-bit FNV-1a of a byte slice. |
| [`hash_string`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/hash/fnv.rs) | `fn hash_string(text: String) -> i64` | 64-bit FNV-1a of a String. |
