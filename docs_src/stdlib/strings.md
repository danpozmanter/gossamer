# `std::strings`

Status: shipped

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
| `starts_with` | fn | Returns whether the string starts with the given prefix. |
| `ends_with` | fn | Returns whether the string ends with the given suffix. |
| `split_once` | fn | Splits on the first occurrence of `sep`; returns Option<(String, String)>. |
| `rsplit_once` | fn | Splits on the last occurrence of `sep`; returns Option<(String, String)>. |
| `count` | fn | Counts non-overlapping occurrences of `needle`. |
| `center` | fn | Symmetric pad to `width` using the given pad character. |
| `slice` | fn | Safe byte-range slice returning Result<String, errors::Error>. |
| `split_whitespace` | fn | Splits on runs of whitespace, dropping empty fields. |
| `trim_start` | fn | Removes leading whitespace. |
| `trim_end` | fn | Removes trailing whitespace. |
| `rfind` | fn | Byte index of the last occurrence of a needle, or -1. |
| `trim_start_matches` | fn | Removes leading characters in the given set. |
| `trim_end_matches` | fn | Removes trailing characters in the given set. |
| `replacen` | fn | Replaces the first n occurrences of a substring. |
| `repeat` | fn | Concatenates n copies of the string. |
| `lines` | fn | Splits into lines, dropping line terminators. |
| `join` | fn | Joins string parts with a separator. |
| `strip_prefix` | fn | Removes a leading prefix if present. |
| `strip_suffix` | fn | Removes a trailing suffix if present. |
| `pad_left` | fn | Left-pads to `width` with the given character. |
| `pad_right` | fn | Right-pads to `width` with the given character. |
| `contains_any` | fn | Reports whether the string contains any rune in a set. |
| `find_any` | fn | Byte index of the first rune in a set, or None. |
| `rfind_any` | fn | Byte index of the last rune in a set, or None. |
| `equal_fold` | fn | Case-insensitive Unicode string equality. |
| `trim_matches` | fn | Removes characters in the given set from both ends. |
| `to_title` | fn | Title-cases the first letter of each word. |

