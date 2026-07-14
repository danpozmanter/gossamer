# `std::regex`

Status: experimental

Compiled regular expressions (Rust `regex` crate syntax; no backreferences or look-around).

## Public items

| Name | Kind | Description |
|---|---|---|
| `Pattern` | type | Compiled pattern handle returned by `compile`. |
| `compile` | fn | Parses a pattern into a reusable `Pattern` or returns an `Err`. |
| `is_match` | fn | Returns whether the pattern matches anywhere in the text. |
| `find` | fn | Returns the first match as `(start, end, text)`, or `None`. |
| `find_all` | fn | Returns every non-overlapping match as `(start, end, text)`. |
| `captures` | fn | Returns capture groups for the first match; index 0 is the full match. |
| `captures_all` | fn | Returns capture groups for every match in the text. |
| `replace` | fn | Replaces the first match with the given replacement (supports `$N`). |
| `replace_all` | fn | Replaces every non-overlapping match. |
| `split` | fn | Splits the text on every pattern match. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/regex.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Pattern`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/regex.rs) | `type` — see the source declaration | Compiled pattern handle returned by `compile`. |
| [`captures`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/regex.rs) | `fn captures(pattern: regex::Pattern, text: String) -> Option<Vec<Option<String>>>` | Returns capture groups for the first match; index 0 is the full match. |
| [`captures_all`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/regex.rs) | `fn captures_all(pattern: regex::Pattern, text: String) -> Vec<Vec<Option<String>>>` | Returns capture groups for every match in the text. |
| [`compile`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/regex.rs) | `fn compile(pattern: String) -> Result<regex::Pattern, errors::Error>` | Parses a pattern into a reusable `Pattern` or returns an `Err`. |
| [`find`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/regex.rs) | `fn find(pattern: regex::Pattern, text: String) -> Option<(i64, i64, String)>` | Returns the first match as `(start, end, text)`, or `None`. |
| [`find_all`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/regex.rs) | `fn find_all(pattern: regex::Pattern, text: String) -> Vec<(i64, i64, String)>` | Returns every non-overlapping match as `(start, end, text)`. |
| [`is_match`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/regex.rs) | `fn is_match(pattern: regex::Pattern, text: String) -> bool` | Returns whether the pattern matches anywhere in the text. |
| [`replace`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/regex.rs) | `fn replace(pattern: regex::Pattern, text: String, replacement: String) -> String` | Replaces the first match with the given replacement (supports `$N`). |
| [`replace_all`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/regex.rs) | `fn replace_all(pattern: regex::Pattern, text: String, replacement: String) -> String` | Replaces every non-overlapping match. |
| [`split`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/regex.rs) | `fn split(pattern: regex::Pattern, text: String) -> Vec<String>` | Splits the text on every pattern match. |
