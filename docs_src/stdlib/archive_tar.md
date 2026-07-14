# `std::archive::tar`

Status: experimental

Unix tar reader and writer (USTAR / PAX-aware decode).

## Public items

| Name | Kind | Description |
|---|---|---|
| `TarEntry` | type | name + data + size + mode. |
| `read` | fn | Reads all entries from a tar archive. |
| `write` | fn | Builds a tar archive from (name, data) pairs. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/archive/tar.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`TarEntry`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/archive/tar.rs) | `type TarEntry` | name + data + size + mode. |
| [`read`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/archive/tar.rs) | `fn read(path: String) -> Result<Vec<(String, Vec<u8>)>, errors::Error>` | Reads all entries from a tar archive. |
| [`write`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/archive/tar.rs) | `fn write(entries: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>, errors::Error>` | Builds a tar archive from (name, data) pairs. |
