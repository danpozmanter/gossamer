# `std::http::multipart`

Status: experimental

RFC 7578 multipart/form-data streaming parser.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/http_multipart.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Config`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/http_multipart.rs) | `type Config` | Per-form size, part-count, and disk-spill limits. |
| [`Form`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/http_multipart.rs) | `type Form` | Parsed multipart envelope: fields + file parts. |
| [`Part`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/http_multipart.rs) | `type Part` | One field or file entry from a multipart body. |
| [`PartData`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/http_multipart.rs) | `type PartData` | In-memory bytes or spilled-to-disk path for a part. |
| [`parse`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/http_multipart.rs) | `fn parse(request: http::Request) -> Result<http::multipart::Form, errors::Error>` | Stream-parse from any Read source into a Form. |
