# `std::path`

Lexical filesystem-path operations; platform path grammar, no URL parsing or I/O.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/path.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Path`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/path.rs) | `type Path` | Immutable UTF-8 lexical path with value-returning operations. |
| [`extension`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/path.rs) | `fn extension(path: String) -> Option<String>` | Dotted extension as an Option. |
| [`file_name`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/path.rs) | `fn file_name(path: String) -> Option<String>` | Final path component, or None. |
| [`file_stem`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/path.rs) | `fn file_stem(path: String) -> Option<String>` | File name without its extension. |
| [`is_absolute`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/path.rs) | `fn is_absolute(path: String) -> bool` | Reports whether the path is absolute. |
| [`join`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/path.rs) | `fn join(base: String, segment: String) -> String` | Joins two path fragments. |
| [`normalize`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/path.rs) | `fn normalize(path: String) -> String` | Lexically normalizes the path. |
| [`parent`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/path.rs) | `fn parent(path: String) -> Option<String>` | Parent directory, or None at the root. |
| [`split`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/path.rs) | `fn split(path: String) -> (String, String)` | Returns (dir, file) for the supplied path. |
| [`starts_with`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/path.rs) | `fn starts_with(path: String, prefix: String) -> bool` | Reports whether the path begins with a prefix component-wise. |

`Path` performs no I/O. Its `join`, `parent`, `file_name`, `stem`,
`extension`, `normalize`, `is_absolute`, and `starts_with` methods return new
values or observations. Keep filesystem access in `std::fs` so errors and
symlink policy stay explicit.
