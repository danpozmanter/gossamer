# `std::unicode`

Unicode character property predicates and casing operations.

## Public items

| Name | Kind | Description |
|---|---|---|
| `is_letter` | fn | True if r is a Unicode letter. |
| `is_digit` | fn | True if r is a decimal digit. |
| `is_number` | fn | True if r is a numeric character. |
| `is_space` | fn | True if r is whitespace. |
| `is_upper` | fn | True if r is an uppercase letter. |
| `is_lower` | fn | True if r is a lowercase letter. |
| `is_title` | fn | True if r is a titlecase letter. |
| `is_punct` | fn | True if r is a punctuation character. |
| `is_symbol` | fn | True if r is a symbol character. |
| `is_mark` | fn | True if r is a combining mark. |
| `is_print` | fn | True if r is a printable character. |
| `is_graphic` | fn | True if r is a graphic character. |
| `is_control` | fn | True if r is a control character. |
| `to_upper` | fn | Maps r to its uppercase equivalent. |
| `to_lower` | fn | Maps r to its lowercase equivalent. |
| `to_title` | fn | Maps r to its titlecase equivalent. |
| `simple_fold` | fn | Next rune in Unicode case-folding cycle. |

