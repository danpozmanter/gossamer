# `std::mime`

Status: shipped

RFC 2045 media type parsing, parameter extraction, and extension lookup.

## Public items

| Name | Kind | Description |
|---|---|---|
| `parse` | fn | Canonical `type/subtype` form of the input, or empty on parse failure. |
| `top` | fn | Top-level type (e.g. `text`) of a media type, or empty. |
| `sub` | fn | Subtype (e.g. `html`) of a media type, or empty. |
| `charset` | fn | Return the `charset` parameter, or empty. |
| `boundary` | fn | Return the multipart `boundary` parameter, or empty. |
| `param` | fn | Return an arbitrary parameter by key, or empty. |
| `type_by_extension` | fn | Canonical media type for a filename extension (dot optional), or empty. |
| `extension_by_type` | fn | Canonical extension (no leading dot) for a media type, or empty. |
| `is_valid` | fn | Return true iff the string parses as a valid media type. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/mime_types.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`boundary`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/mime_types.rs) | `fn boundary(mime: String) -> String` | Return the multipart `boundary` parameter, or empty. |
| [`charset`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/mime_types.rs) | `fn charset(mime: String) -> String` | Return the `charset` parameter, or empty. |
| [`extension_by_type`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/mime_types.rs) | `fn extension_by_type(mime: String) -> Option<String>` | Canonical extension (no leading dot) for a media type, or empty. |
| [`is_valid`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/mime_types.rs) | `fn is_valid(value: String) -> bool` | Return true iff the string parses as a valid media type. |
| [`param`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/mime_types.rs) | `fn param(mime: String, name: String) -> Option<String>` | Return an arbitrary parameter by key, or empty. |
| [`parse`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/mime_types.rs) | `fn parse(value: String) -> Result<mime::Mime, errors::Error>` | Canonical `type/subtype` form of the input, or empty on parse failure. |
| [`sub`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/mime_types.rs) | `fn sub(mime: String) -> String` | Subtype (e.g. `html`) of a media type, or empty. |
| [`top`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/mime_types.rs) | `fn top(mime: String) -> String` | Top-level type (e.g. `text`) of a media type, or empty. |
| [`type_by_extension`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/mime_types.rs) | `fn type_by_extension(ext: String) -> Option<String>` | Canonical media type for a filename extension (dot optional), or empty. |
