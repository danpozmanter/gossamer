# `std::hash::adler32`

Status: experimental

Adler-32 checksums.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/hash/adler32.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`checksum`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/hash/adler32.rs) | `fn checksum(data: Vec<u8>) -> i64` | Adler-32 checksum of a byte slice. |
| [`checksum_string`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/hash/adler32.rs) | `fn checksum_string(text: String) -> i64` | Adler-32 checksum of a String. |
| [`update`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/hash/adler32.rs) | `fn update(seed: i64, data: Vec<u8>) -> i64` | Continues an Adler-32 from a running value over more bytes. |
