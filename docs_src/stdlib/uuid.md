# `std::uuid`

UUID v4 (random) and v7 (timestamp-ordered) generation, parse, and normalize.

## Public items

| Name | Kind | Description |
|---|---|---|
| `v4` | fn | Generate a fresh random v4 UUID as a canonical hyphenated string. |
| `v7` | fn | Generate a fresh v7 (timestamp-ordered) UUID. |
| `is_valid` | fn | Return true iff the string parses as a canonical UUID. |
| `normalize` | fn | Lowercase canonical UUID form of the input, or empty string on parse failure. |
| `simple` | fn | 32-character unhyphenated form of the input, or empty string on parse failure. |

