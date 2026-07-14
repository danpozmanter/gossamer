# `std::http::static_files`

Status: experimental

Caching static-file handler: ETag, Last-Modified, byte ranges, MIME sniff.

## Public items

| Name | Kind | Description |
|---|---|---|
| `FileServer` | type | Static-file handler rooted at a directory (Rust-side; streaming). |
| `serve_file` | fn | Read a single file and return it as a Response struct. Interp tier. |
| `mime_for_path` | fn | Guess a MIME type from a file path's extension. Available in interp + compiled. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_static_files.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`FileServer`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_static_files.rs) | `type` — see the source declaration | Static-file handler rooted at a directory (Rust-side; streaming). |
| [`mime_for_path`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_static_files.rs) | `fn mime_for_path(path: String) -> String` | Guess a MIME type from a file path's extension. Available in interp + compiled. |
| [`serve_file`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_static_files.rs) | `fn serve_file(path: String) -> Result<http::Response, errors::Error>` | Read a single file and return it as a Response struct. Interp tier. |
