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
| `parse` | fn | Stream-parse from any Read source into a Form. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_multipart.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Config`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_multipart.rs) | `type` — see the source declaration | Per-form size, part-count, and disk-spill limits. |
| [`Form`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_multipart.rs) | `type` — see the source declaration | Parsed multipart envelope: fields + file parts. |
| [`Part`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_multipart.rs) | `type` — see the source declaration | One field or file entry from a multipart body. |
| [`PartData`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_multipart.rs) | `type` — see the source declaration | In-memory bytes or spilled-to-disk path for a part. |
| [`parse`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_multipart.rs) | `fn parse(request: http::Request) -> Result<http::multipart::Form, errors::Error>` | Stream-parse from any Read source into a Form. |
