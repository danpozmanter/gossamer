# `std::bufio`

Status: experimental

Buffered readers, writers, and line scanners.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Reader` | type | Buffered reader. |
| `Writer` | type | Buffered writer. |
| `Scanner` | type | Line / token scanner. |
| `read_lines` | fn | Reads every line from a file path; one-shot convenience over the streaming Scanner. |
| `read_lines_of` | fn | Reads every line of a file path into a Vec<String>. |
| `read_to_string` | fn | Reads an entire file path into a String. |
| `split_whitespace` | fn | Splits a String on runs of whitespace. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/bufio.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Reader`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/bufio.rs) | `type Reader` | Buffered reader. |
| [`Scanner`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/bufio.rs) | `type Scanner` | Line / token scanner. |
| [`Writer`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/bufio.rs) | `type Writer` | Buffered writer. |
| [`read_lines`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/bufio.rs) | `fn read_lines(path: String) -> Result<Vec<String>, io::Error>` | Reads every line from a file path; one-shot convenience over the streaming Scanner. |
| [`read_lines_of`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/bufio.rs) | `fn read_lines_of(path: String) -> Result<Vec<String>, io::Error>` | Reads every line of a file path into a Vec<String>. |
| [`read_to_string`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/bufio.rs) | `fn read_to_string(path: String) -> Result<String, io::Error>` | Reads an entire file path into a String. |
| [`split_whitespace`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/bufio.rs) | `fn split_whitespace(text: String) -> Vec<String>` | Splits a String on runs of whitespace. |
