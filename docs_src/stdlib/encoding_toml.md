# `std::encoding::toml`

TOML 1.0 parsing + emission. Pair with `<Type>::from_toml` for typed decoding (struct auto-derive).

## Public items

| Name | Kind | Description |
|---|---|---|
| `to_json` | fn | Convert TOML text to JSON text; returns Result<String, errors::Error>. |
| `from_json` | fn | Render JSON text as TOML text; returns Result<String, errors::Error>. |
| `is_valid` | fn | Return true iff the string parses as TOML. |
| `pretty` | fn | Round-trip TOML through the pretty-printer. |

