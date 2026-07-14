# `std::archive::zip`

Status: experimental

ZIP archive reader and writer.

## Public items

| Name | Kind | Description |
|---|---|---|
| `ZipEntry` | type | name + decompressed data + is_dir flag. |
| `read` | fn | Reads all file entries from a zip stored in `data`. |
| `write` | fn | Builds an in-memory zip from (name, data) pairs. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/archive/zip.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`ZipEntry`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/archive/zip.rs) | `type` — see the source declaration | name + decompressed data + is_dir flag. |
| [`read`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/archive/zip.rs) | `fn read(path: String) -> Result<Vec<(String, Vec<u8>)>, errors::Error>` | Reads all file entries from a zip stored in `data`. |
| [`write`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/archive/zip.rs) | `fn write(entries: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>, errors::Error>` | Builds an in-memory zip from (name, data) pairs. |
