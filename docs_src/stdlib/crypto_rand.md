# `std::crypto::rand`

Status: experimental

Secure random bytes from the host CSPRNG.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/crypto.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`bytes`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/crypto.rs) | `fn bytes(n: i64) -> Result<Vec<u8>, errors::Error>` | Returns a fresh random byte vector. |
