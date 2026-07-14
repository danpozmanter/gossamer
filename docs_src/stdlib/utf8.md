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

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf8.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`append_rune`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf8.rs) | `fn append_rune(bytes: Vec<u8>, rune: char) -> Vec<u8>` | Appends the UTF-8 encoding of a rune to a byte Vec. |
| [`decode_last_rune`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf8.rs) | `fn decode_last_rune(bytes: Vec<u8>) -> (char, i64)` | Decodes the last rune from bytes, returning (rune, width). |
| [`decode_last_rune_in_string`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf8.rs) | `fn decode_last_rune_in_string(text: String) -> (char, i64)` | Decodes the last rune from a String, returning (rune, width). |
| [`decode_rune`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf8.rs) | `fn decode_rune(bytes: Vec<u8>) -> (char, i64)` | Decodes the first rune from bytes, returning (rune, width). |
| [`decode_rune_in_string`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf8.rs) | `fn decode_rune_in_string(text: String) -> (char, i64)` | Decodes the first rune from a String, returning (rune, width). |
| [`full_rune`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf8.rs) | `fn full_rune(bytes: Vec<u8>) -> bool` | Reports whether the bytes begin with a full rune. |
| [`full_rune_in_string`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf8.rs) | `fn full_rune_in_string(text: String) -> bool` | Reports whether the String begins with a full rune. |
| [`is_valid`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf8.rs) | `fn is_valid(bytes: Vec<u8>) -> bool` | Validates a byte slice as UTF-8. |
| [`rune_count`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf8.rs) | `fn rune_count(bytes: Vec<u8>) -> i64` | Counts Unicode scalar values. |
| [`rune_count_in_string`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf8.rs) | `fn rune_count_in_string(text: String) -> i64` | Counts the runes in a String. |
| [`rune_len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf8.rs) | `fn rune_len(rune: char) -> i64` | Number of bytes needed to encode a rune. |
| [`rune_start`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf8.rs) | `fn rune_start(byte: u8) -> bool` | Reports whether the byte could be the first of a rune. |
| [`valid_rune`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf8.rs) | `fn valid_rune(rune: char) -> bool` | Reports whether a code point can be legally encoded. |
| [`valid_string`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf8.rs) | `fn valid_string(text: String) -> bool` | Reports whether a String is valid UTF-8. |
