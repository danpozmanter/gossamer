# `std::unicode`

Status: experimental

Unicode general-category predicates, casing, normalization, and segmentation.

## Public items

| Name | Kind | Description |
|---|---|---|
| `is_letter` | fn | True if r is in general-category group L. |
| `is_digit` | fn | True if r is a decimal digit (category Nd). |
| `is_number` | fn | True if r is any numeric (Nd\|Nl\|No). |
| `is_space` | fn | True if r is whitespace (Z* plus HT/LF/VT/FF/CR/NEL). |
| `is_upper` | fn | True if r is category Lu. |
| `is_lower` | fn | True if r is category Ll. |
| `is_title` | fn | True if r is category Lt. |
| `is_punct` | fn | True if r is in general-category group P. |
| `is_symbol` | fn | True if r is in general-category group S. |
| `is_mark` | fn | True if r is in general-category group M. |
| `is_print` | fn | True if r is printable (not Cc/Cf/Cs/Co/Cn). |
| `is_graphic` | fn | True if r is graphic (printable and not whitespace). |
| `is_control` | fn | True if r is category Cc. |
| `is_assigned` | fn | True if r is an assigned code point (not Cn). |
| `to_upper` | fn | Simple uppercase mapping for one rune. |
| `to_lower` | fn | Simple lowercase mapping for one rune. |
| `to_title` | fn | Simple titlecase mapping for one rune. |
| `simple_fold` | fn | Next rune in Unicode case-folding cycle. |
| `combining_class` | fn | Canonical combining class (0-254) for r. |
| `to_upper_str` | fn | Full uppercase mapping for a string (ss -> SS). |
| `to_lower_str` | fn | Full lowercase mapping for a string. |
| `fold_case` | fn | Simple case-folded comparison form for a string. |
| `nfc` | fn | Normalize a string to NFC (canonical composition). |
| `nfd` | fn | Normalize a string to NFD (canonical decomposition). |
| `nfkc` | fn | Normalize a string to NFKC (compat composition). |
| `nfkd` | fn | Normalize a string to NFKD (compat decomposition). |
| `is_nfc` | fn | True if a string is already in NFC. |
| `is_nfd` | fn | True if a string is already in NFD. |
| `is_nfkc` | fn | True if a string is already in NFKC. |
| `is_nfkd` | fn | True if a string is already in NFKD. |
| `graphemes` | fn | UAX #29 extended grapheme clusters of a string. |
| `grapheme_count` | fn | Number of UAX #29 grapheme clusters in a string. |
| `words` | fn | UAX #29 Unicode words in a string (skips punct/whitespace). |
| `word_bounds` | fn | UAX #29 word boundaries (includes punct + whitespace runs). |
| `word_count` | fn | Number of UAX #29 words in a string. |
| `sentences` | fn | UAX #29 Unicode sentences in a string. |
| `sentence_count` | fn | Number of UAX #29 sentences in a string. |

