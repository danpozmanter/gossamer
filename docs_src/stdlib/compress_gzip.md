# `std::compress::gzip`

Status: experimental

gzip encoder / decoder (RFC 1952; flate2-backed).

## Public items

| Name | Kind | Description |
|---|---|---|
| `Level` | type | Compression level (`0` store-only … `9` best); default is gzip(1)'s `6`. |
| `encode` | fn | Compresses bytes at the supplied Level. |
| `decode` | fn | Decompresses a gzip-formatted payload. |

