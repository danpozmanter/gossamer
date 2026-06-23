# `std::utf8`

Status: shipped

UTF-8 validation and scalar decoding.

## Public items

| Name | Kind | Description |
|---|---|---|
| `is_valid` | fn | Validates a byte slice as UTF-8. |
| `rune_count` | fn | Counts Unicode scalar values. |
| `rune_count_in_string` | fn | Counts the runes in a String. |
| `rune_len` | fn | Number of bytes needed to encode a rune. |
| `valid_string` | fn | Reports whether a String is valid UTF-8. |
| `valid_rune` | fn | Reports whether a code point can be legally encoded. |
| `full_rune` | fn | Reports whether the bytes begin with a full rune. |
| `full_rune_in_string` | fn | Reports whether the String begins with a full rune. |
| `rune_start` | fn | Reports whether the byte could be the first of a rune. |
| `decode_rune` | fn | Decodes the first rune from bytes, returning (rune, width). |
| `decode_rune_in_string` | fn | Decodes the first rune from a String, returning (rune, width). |
| `decode_last_rune` | fn | Decodes the last rune from bytes, returning (rune, width). |
| `decode_last_rune_in_string` | fn | Decodes the last rune from a String, returning (rune, width). |
| `append_rune` | fn | Appends the UTF-8 encoding of a rune to a byte Vec. |

