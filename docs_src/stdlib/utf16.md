# `std::utf16`

Status: unproven

UTF-16 encoding/decoding and surrogate pair helpers.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf16.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`decode_surrogate_pair`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf16.rs) | `fn decode_surrogate_pair(high: char, low: char) -> Result<char, errors::Error>` | Decodes a high+low surrogate pair to a char. |
| [`decode_to_string`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf16.rs) | `fn decode_to_string(units: Vec<u16>) -> Result<String, errors::Error>` | Decodes a []u16 to String. |
| [`encode_string`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf16.rs) | `fn encode_string(text: String) -> Vec<u16>` | Encodes a String directly to Vec<u16>. |
| [`is_surrogate`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf16.rs) | `fn is_surrogate(rune: char) -> bool` | True iff r falls in the surrogate range U+D800..U+DFFF. |
| [`rune_len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/utf16.rs) | `fn rune_len(rune: char) -> i64` | Number of UTF-16 code units needed to encode r (1 or 2). |
