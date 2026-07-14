# `std::flag`

Status: shipped

Batteries-included CLI argument parsing.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Set` | type | Flag definition + parsing set. |
| `Error` | type | Error produced while parsing flags. |
| `parse` | fn | Parses the default flag set against the given args. |
| `string` | fn | Defines a string flag on the default set. |
| `int` | fn | Defines an integer flag on the default set. |
| `bool` | fn | Defines a boolean flag on the default set. |
| `define` | fn | Registers a flag definition on the default set. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/flag.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Error`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/flag.rs) | `type` — see the source declaration | Error produced while parsing flags. |
| [`Set`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/flag.rs) | `type` — see the source declaration | Flag definition + parsing set. |
| [`bool`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/flag.rs) | `fn bool(name: String, default: bool, usage: String, short: char) -> flag::Flag` | Defines a boolean flag on the default set. |
| [`define`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/flag.rs) | `fn define(name: String, flags: Vec<flag::Flag>) -> flag::FlagSet` | Registers a flag definition on the default set. |
| [`int`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/flag.rs) | `fn int(name: String, default: i64, usage: String, short: char) -> flag::Flag` | Defines an integer flag on the default set. |
| [`parse`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/flag.rs) | `fn parse(args: Vec<String>) -> Result<Vec<String>, errors::Error>` | Parses the default flag set against the given args. |
| [`string`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/flag.rs) | `fn string(name: String, default: String, usage: String, short: char) -> flag::Flag` | Defines a string flag on the default set. |
