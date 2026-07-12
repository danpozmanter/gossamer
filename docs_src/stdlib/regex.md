# `std::regex`

Status: shipped

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

