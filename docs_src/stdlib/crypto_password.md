# `std::crypto::password`

Status: experimental

Argon2id password hashing facade: PHC-string hash / verify / re-hash policy.

## Public items

| Name | Kind | Description |
|---|---|---|
| `hash` | fn | Argon2id hash of plaintext; returns a PHC-format string for storage. |
| `verify` | fn | Constant-time verify of plaintext against a stored PHC string. |
| `needs_rehash` | fn | True iff the stored PHC's parameters are below the current defaults. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/crypto.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`hash`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/crypto.rs) | `fn hash(password: String) -> Result<String, errors::Error>` | Argon2id hash of plaintext; returns a PHC-format string for storage. |
| [`needs_rehash`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/crypto.rs) | `fn needs_rehash(hash: String) -> bool` | True iff the stored PHC's parameters are below the current defaults. |
| [`verify`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/crypto.rs) | `fn verify(password: String, hash: String) -> Result<bool, errors::Error>` | Constant-time verify of plaintext against a stored PHC string. |
