# `std::archive::tar`

Unix tar reader and writer (USTAR / PAX-aware decode).

## Public items

| Name | Kind | Description |
|---|---|---|
| `TarEntry` | type | name + data + size + mode. |
| `read` | fn | Reads all entries from a tar archive. |
| `write` | fn | Builds a tar archive from (name, data) pairs. |

