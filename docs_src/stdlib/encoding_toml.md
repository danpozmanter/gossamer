# `std::encoding::toml`

Status: experimental

TOML 1.0 parsing + emission. Pair with the turbofish `from_toml::<Type>` for typed decoding (struct auto-derive).

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding_toml.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`from_json`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding_toml.rs) | `fn from_json(source: String) -> Result<String, errors::Error>` | Render JSON text as TOML text; returns Result<String, errors::Error>. |
| [`is_valid`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding_toml.rs) | `fn is_valid(source: String) -> bool` | Return true iff the string parses as TOML. |
| [`pretty`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding_toml.rs) | `fn pretty(source: String) -> Result<String, errors::Error>` | Round-trip TOML through the pretty-printer. |
| [`to_json`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding_toml.rs) | `fn to_json(source: String) -> Result<String, errors::Error>` | Convert TOML text to JSON text; returns Result<String, errors::Error>. |
