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

