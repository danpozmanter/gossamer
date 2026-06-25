# `std::utf16`

Status: shipped

UTF-16 encoding/decoding and surrogate pair helpers.

## Public items

| Name | Kind | Description |
|---|---|---|
| `is_surrogate` | fn | True iff r falls in the surrogate range U+D800..U+DFFF. |
| `rune_len` | fn | Number of UTF-16 code units needed to encode r (1 or 2). |
| `decode_surrogate_pair` | fn | Decodes a high+low surrogate pair to a char. |
| `encode_string` | fn | Encodes a String directly to Vec<u16>. |
| `decode_to_string` | fn | Decodes a []u16 to String. |

