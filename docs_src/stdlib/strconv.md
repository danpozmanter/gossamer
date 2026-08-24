# `std::strconv`

Status: experimental

Conversions between strings and primitive numeric types.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/strconv.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`format_f64`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/strconv.rs) | `fn format_f64(value: f64) -> String` | Renders an `f64` as a decimal string. |
| [`format_i64`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/strconv.rs) | `fn format_i64(value: i64) -> String` | Renders an `i64` as a decimal string. |
| [`format_i64_radix`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/strconv.rs) | `fn format_i64_radix(value: i64, base: i64) -> String` | Formats an i64 in the given base (2..=36). |
| [`parse_bool`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/strconv.rs) | `fn parse_bool(text: String) -> Result<bool, strconv::ParseError>` | Parses `"true"` / `"false"` into a bool. |
| [`parse_f64`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/strconv.rs) | `fn parse_f64(text: String) -> Result<f64, strconv::ParseError>` | Parses a decimal `f64`. |
| [`parse_i64`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/strconv.rs) | `fn parse_i64(text: String) -> Result<i64, strconv::ParseError>` | Parses a decimal `i64`. |
| [`parse_i64_radix`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/strconv.rs) | `fn parse_i64_radix(text: String, base: i64) -> Result<i64, strconv::ParseError>` | Parses an i64 from a string in the given base (2..=36). |
| [`parse_u64`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/strconv.rs) | `fn parse_u64(text: String) -> Result<u64, strconv::ParseError>` | Parses a decimal `u64`. |
| [`quote`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/strconv.rs) | `fn quote(text: String) -> String` | Wraps a string in double quotes with escapes. |
| [`unquote`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/strconv.rs) | `fn unquote(text: String) -> Result<String, strconv::ParseError>` | Removes surrounding quotes and resolves escapes. |
