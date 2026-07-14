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

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`combining_class`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn combining_class(rune: char) -> i64` | Canonical combining class (0-254) for r. |
| [`fold_case`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn fold_case(text: String) -> String` | Simple case-folded comparison form for a string. |
| [`grapheme_count`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn grapheme_count(text: String) -> i64` | Number of UAX #29 grapheme clusters in a string. |
| [`graphemes`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn graphemes(text: String) -> Vec<String>` | UAX #29 extended grapheme clusters of a string. |
| [`is_assigned`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn is_assigned(rune: char) -> bool` | True if r is an assigned code point (not Cn). |
| [`is_control`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn is_control(rune: char) -> bool` | True if r is category Cc. |
| [`is_digit`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn is_digit(rune: char) -> bool` | True if r is a decimal digit (category Nd). |
| [`is_graphic`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn is_graphic(rune: char) -> bool` | True if r is graphic (printable and not whitespace). |
| [`is_letter`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn is_letter(rune: char) -> bool` | True if r is in general-category group L. |
| [`is_lower`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn is_lower(rune: char) -> bool` | True if r is category Ll. |
| [`is_mark`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn is_mark(rune: char) -> bool` | True if r is in general-category group M. |
| [`is_nfc`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn is_nfc(text: String) -> bool` | True if a string is already in NFC. |
| [`is_nfd`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn is_nfd(text: String) -> bool` | True if a string is already in NFD. |
| [`is_nfkc`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn is_nfkc(text: String) -> bool` | True if a string is already in NFKC. |
| [`is_nfkd`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn is_nfkd(text: String) -> bool` | True if a string is already in NFKD. |
| [`is_number`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn is_number(rune: char) -> bool` | True if r is any numeric (Nd\|Nl\|No). |
| [`is_print`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn is_print(rune: char) -> bool` | True if r is printable (not Cc/Cf/Cs/Co/Cn). |
| [`is_punct`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn is_punct(rune: char) -> bool` | True if r is in general-category group P. |
| [`is_space`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn is_space(rune: char) -> bool` | True if r is whitespace (Z* plus HT/LF/VT/FF/CR/NEL). |
| [`is_symbol`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn is_symbol(rune: char) -> bool` | True if r is in general-category group S. |
| [`is_title`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn is_title(rune: char) -> bool` | True if r is category Lt. |
| [`is_upper`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn is_upper(rune: char) -> bool` | True if r is category Lu. |
| [`nfc`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn nfc(text: String) -> String` | Normalize a string to NFC (canonical composition). |
| [`nfd`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn nfd(text: String) -> String` | Normalize a string to NFD (canonical decomposition). |
| [`nfkc`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn nfkc(text: String) -> String` | Normalize a string to NFKC (compat composition). |
| [`nfkd`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn nfkd(text: String) -> String` | Normalize a string to NFKD (compat decomposition). |
| [`sentence_count`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn sentence_count(text: String) -> i64` | Number of UAX #29 sentences in a string. |
| [`sentences`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn sentences(text: String) -> Vec<String>` | UAX #29 Unicode sentences in a string. |
| [`simple_fold`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn simple_fold(rune: char) -> char` | Next rune in Unicode case-folding cycle. |
| [`to_lower`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn to_lower(rune: char) -> char` | Simple lowercase mapping for one rune. |
| [`to_lower_str`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn to_lower_str(text: String) -> String` | Full lowercase mapping for a string. |
| [`to_title`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn to_title(rune: char) -> char` | Simple titlecase mapping for one rune. |
| [`to_upper`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn to_upper(rune: char) -> char` | Simple uppercase mapping for one rune. |
| [`to_upper_str`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn to_upper_str(text: String) -> String` | Full uppercase mapping for a string (ss -> SS). |
| [`word_bounds`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn word_bounds(text: String) -> Vec<(i64, i64)>` | UAX #29 word boundaries (includes punct + whitespace runs). |
| [`word_count`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn word_count(text: String) -> i64` | Number of UAX #29 words in a string. |
| [`words`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/unicode.rs) | `fn words(text: String) -> Vec<String>` | UAX #29 Unicode words in a string (skips punct/whitespace). |
