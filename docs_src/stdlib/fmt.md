# `std::fmt`

Status: experimental

Formatted printing and string interpolation.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/fmt.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Debug`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/fmt.rs) | `trait Debug` | Trait for debugging-oriented string conversion. |
| [`Display`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/fmt.rs) | `trait Display` | Trait for human-readable string conversion. |
| [`eprint`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/fmt.rs) | `builtin eprint(...)` | Prints to stderr without a trailing newline. |
| [`eprintln`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/fmt.rs) | `builtin eprintln(...)` | Prints to stderr followed by a newline. |
| [`format`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/fmt.rs) | `builtin format(...)` | Formats arguments into an owned `String`. |
| [`print`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/fmt.rs) | `builtin print(...)` | Prints to stdout without a trailing newline. |
| [`println`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/fmt.rs) | `builtin println(...)` | Prints to stdout followed by a newline. |
