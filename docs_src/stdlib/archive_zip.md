# `std::archive::zip`

ZIP archive reader and writer.

## Public items

| Name | Kind | Description |
|---|---|---|
| `ZipEntry` | type | name + decompressed data + is_dir flag. |
| `read` | fn | Reads all file entries from a zip stored in `data`. |
| `write` | fn | Builds an in-memory zip from (name, data) pairs. |

