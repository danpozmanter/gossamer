# `std::fmt`

Status: shipped

Formatted printing and string interpolation.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Display` | trait | Trait for human-readable string conversion. |
| `Debug` | trait | Trait for debugging-oriented string conversion. |
| `println` | macro | Prints to stdout followed by a newline. |
| `print` | macro | Prints to stdout without a trailing newline. |
| `eprintln` | macro | Prints to stderr followed by a newline. |
| `eprint` | macro | Prints to stderr without a trailing newline. |
| `format` | macro | Formats arguments into an owned `String`. |
| `write` | macro | Writes formatted output into a `Writer`. |
| `writeln` | macro | Writes formatted output into a `Writer` followed by a newline. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fmt.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Debug`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fmt.rs) | `trait` — see the source declaration | Trait for debugging-oriented string conversion. |
| [`Display`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fmt.rs) | `trait` — see the source declaration | Trait for human-readable string conversion. |
| [`eprint`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fmt.rs) | `macro` — see the source declaration | Prints to stderr without a trailing newline. |
| [`eprintln`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fmt.rs) | `macro` — see the source declaration | Prints to stderr followed by a newline. |
| [`format`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fmt.rs) | `macro` — see the source declaration | Formats arguments into an owned `String`. |
| [`print`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fmt.rs) | `macro` — see the source declaration | Prints to stdout without a trailing newline. |
| [`println`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fmt.rs) | `macro` — see the source declaration | Prints to stdout followed by a newline. |
| [`write`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fmt.rs) | `macro` — see the source declaration | Writes formatted output into a `Writer`. |
| [`writeln`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fmt.rs) | `macro` — see the source declaration | Writes formatted output into a `Writer` followed by a newline. |
