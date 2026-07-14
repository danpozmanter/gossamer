# `std::hash::crc32`

Status: experimental

CRC-32 (IEEE) checksums.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/hash/crc32.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`checksum`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/hash/crc32.rs) | `fn checksum(data: Vec<u8>) -> i64` | CRC-32 checksum of a byte slice. |
| [`checksum_string`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/hash/crc32.rs) | `fn checksum_string(text: String) -> i64` | CRC-32 checksum of a String. |
| [`update`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/hash/crc32.rs) | `fn update(seed: i64, data: Vec<u8>) -> i64` | Continues a CRC-32 from a running value over more bytes. |
