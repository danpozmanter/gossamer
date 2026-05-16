# `std::strings`

Polished `String` operations.

## Public items

| Name | Kind | Description |
|---|---|---|
| `split` | fn | Splits a string by a delimiter. |
| `splitn` | fn | Splits a string into at most `n` parts. |
| `trim` | fn | Removes leading and trailing whitespace. |
| `contains` | fn | Returns whether the string contains a substring. |
| `find` | fn | Returns the byte position of the first match. |
| `replace` | fn | Replaces every occurrence of `from` with `to`. |
| `to_lower` | fn | Lowercases every character. |
| `to_upper` | fn | Uppercases every character. |
| `to_lowercase` | fn | Alias for to_lower (Rust-style name). |
| `to_uppercase` | fn | Alias for to_upper (Rust-style name). |
| `starts_with` | fn | Returns whether the string starts with the given prefix. |
| `ends_with` | fn | Returns whether the string ends with the given suffix. |
| `split_once` | fn | Splits on the first occurrence of `sep`; returns Option<(String, String)>. |
| `rsplit_once` | fn | Splits on the last occurrence of `sep`; returns Option<(String, String)>. |
| `count` | fn | Counts non-overlapping occurrences of `needle`. |
| `strip_chars` | fn | Trims any character in `cutset` from both ends. |
| `lstrip_chars` | fn | Trims any character in `cutset` from the left end. |
| `rstrip_chars` | fn | Trims any character in `cutset` from the right end. |
| `zfill` | fn | Pads with `'0'` on the left until at least `width` wide. |
| `center` | fn | Symmetric pad to `width` using the given pad character. |
| `slice` | fn | Safe byte-range slice returning Result<String, errors::Error>. |

