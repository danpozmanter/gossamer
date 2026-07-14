# `std::strings`

Status: experimental

String operations.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`bytes`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn bytes(text: String) -> Vec<u8>` | Returns the UTF-8 bytes of the string. |
| [`center`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn center(text: String, width: i64, fill: char) -> String` | Symmetric pad to `width` using the given pad character. |
| [`chars`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn chars(text: String) -> Vec<char>` | Returns the Unicode scalar values of the string. |
| [`contains`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn contains(text: String, needle: String | char) -> bool` | Returns whether the string contains a substring. |
| [`contains_any`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn contains_any(text: String, needle: String | char) -> bool` | Reports whether the string contains any rune in a set. |
| [`count`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn count(text: String, needle: String | char) -> i64` | Counts non-overlapping occurrences of `needle`. |
| [`ends_with`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn ends_with(text: String, needle: String | char) -> bool` | Returns whether the string ends with the given suffix. |
| [`equal_fold`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn equal_fold(text: String, needle: String | char) -> bool` | Case-insensitive Unicode string equality. |
| [`find`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn find(text: String, needle: String | char) -> Option<i64>` | Returns the byte position of the first match. |
| [`find_any`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn find_any(text: String, needle: String | char) -> Option<i64>` | Byte index of the first rune in a set, or None. |
| [`join`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn join(parts: Vec<String>, sep: String) -> String` | Joins string parts with a separator. |
| [`lines`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn lines(text: String) -> Vec<String>` | Splits into lines, dropping line terminators. |
| [`pad_left`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn pad_left(text: String, width: i64, fill: char) -> String` | Left-pads to `width` with the given character. |
| [`pad_right`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn pad_right(text: String, width: i64, fill: char) -> String` | Right-pads to `width` with the given character. |
| [`repeat`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn repeat(text: String, count: i64) -> String` | Concatenates n copies of the string. |
| [`replace`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn replace(text: String, from: String, to: String) -> String` | Replaces every occurrence of `from` with `to`. |
| [`replacen`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn replacen(text: String, from: String, to: String, n: i64) -> String` | Replaces the first n occurrences of a substring. |
| [`rfind`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn rfind(text: String, needle: String | char) -> Option<i64>` | Byte index of the last occurrence of a needle, or -1. |
| [`rfind_any`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn rfind_any(text: String, needle: String | char) -> Option<i64>` | Byte index of the last rune in a set, or None. |
| [`rsplit_once`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn rsplit_once(text: String, sep: String) -> Option<(String, String)>` | Splits on the last occurrence of `sep`; returns Option<(String, String)>. |
| [`slice`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn slice(text: String, start: i64, end: i64) -> Result<String, errors::Error>` | Safe byte-range slice returning Result<String, errors::Error>. |
| [`split`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn split(text: String, sep: String) -> Vec<String>` | Splits a string by a delimiter. |
| [`split_once`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn split_once(text: String, sep: String) -> Option<(String, String)>` | Splits on the first occurrence of `sep`; returns Option<(String, String)>. |
| [`split_whitespace`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn split_whitespace(text: String) -> Vec<String>` | Splits on runs of whitespace, dropping empty fields. |
| [`splitn`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn splitn(text: String, n: i64, sep: String) -> Vec<String>` | Splits a string into at most `n` parts. |
| [`starts_with`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn starts_with(text: String, needle: String | char) -> bool` | Returns whether the string starts with the given prefix. |
| [`strip_prefix`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn strip_prefix(text: String, prefix: String) -> Option<String>` | Removes a leading prefix if present. |
| [`strip_suffix`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn strip_suffix(text: String, prefix: String) -> Option<String>` | Removes a trailing suffix if present. |
| [`to_bool`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn to_bool(text: String) -> Result<bool, errors::Error>` | Parses exactly `true` / `false` to Option<bool>. |
| [`to_f64`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn to_f64(text: String) -> Result<f64, errors::Error>` | Strict full-string parse to Option<f64>. |
| [`to_i64`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn to_i64(text: String) -> Result<i64, errors::Error>` | Strict full-string parse to Option<i64>. |
| [`to_lowercase`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn to_lowercase(text: String) -> String` | Lowercases every character. |
| [`to_title`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn to_title(text: String) -> String` | Title-cases the first letter of each word. |
| [`to_uppercase`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn to_uppercase(text: String) -> String` | Uppercases every character. |
| [`trim`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn trim(text: String) -> String` | Removes leading and trailing whitespace. |
| [`trim_end`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn trim_end(text: String) -> String` | Removes trailing whitespace. |
| [`trim_end_matches`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn trim_end_matches(text: String, cutset: String) -> String` | Removes trailing characters in the given set. |
| [`trim_matches`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn trim_matches(text: String, cutset: String) -> String` | Removes characters in the given set from both ends. |
| [`trim_start`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn trim_start(text: String) -> String` | Removes leading whitespace. |
| [`trim_start_matches`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/strings.rs) | `fn trim_start_matches(text: String, cutset: String) -> String` | Removes leading characters in the given set. |
