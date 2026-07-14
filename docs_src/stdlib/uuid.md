# `std::uuid`

Status: experimental

UUID v4 (random) and v7 (timestamp-ordered) generation, parse, and normalize.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/uuid.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`is_valid`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/uuid.rs) | `fn is_valid(text: String) -> bool` | Return true iff the string parses as a canonical UUID. |
| [`normalize`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/uuid.rs) | `fn normalize(text: String) -> String` | Lowercase canonical UUID form of the input, or empty string on parse failure. |
| [`simple`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/uuid.rs) | `fn simple(text: String) -> String` | 32-character unhyphenated form of the input, or empty string on parse failure. |
| [`v4`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/uuid.rs) | `fn v4() -> String` | Generate a fresh random v4 UUID as a canonical hyphenated string. |
| [`v7`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/uuid.rs) | `fn v7() -> String` | Generate a fresh v7 (timestamp-ordered) UUID. |
