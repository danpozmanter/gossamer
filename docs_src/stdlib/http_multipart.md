# `std::http::multipart`

Status: shipped

RFC 7578 multipart/form-data streaming parser.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Config` | type | Per-form size, part-count, and disk-spill limits. |
| `Part` | type | One field or file entry from a multipart body. |
| `PartData` | type | In-memory bytes or spilled-to-disk path for a part. |
| `Form` | type | Parsed multipart envelope: fields + file parts. |
| `parse_boundary` | fn | Extract the boundary token from a Content-Type header. |
| `parse_bytes` | fn | Parse a full body buffer into a Form. |
| `parse` | fn | Stream-parse from any Read source into a Form. |

