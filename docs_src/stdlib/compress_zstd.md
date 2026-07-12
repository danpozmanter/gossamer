# `std::compress::zstd`

Status: experimental

Zstandard encoder / decoder (RFC 8478; libzstd-vendored).

## Public items

| Name | Kind | Description |
|---|---|---|
| `encode` | fn | One-shot Zstandard compress at the default level (3). |
| `encode_level` | fn | One-shot Zstandard compress at the supplied level (1 fastest -- 22 best). |
| `decode` | fn | One-shot Zstandard decompress. |

