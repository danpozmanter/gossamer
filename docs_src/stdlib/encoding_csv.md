# `std::encoding::csv`

Status: experimental

CSV record reader and writer.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`parse_line`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn parse_line(line: String) -> Vec<String>` | Parses a single CSV-formatted line. |
| [`read`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn read(text: String) -> Result<Vec<Vec<String>>, errors::Error>` | Parses all CSV records from a string. |
| [`write`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/encoding.rs) | `fn write(rows: Vec<Vec<String>>) -> String` | Serialises records as a CSV string. |
