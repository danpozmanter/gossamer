# `std::http::static_files`

Status: experimental

Caching static-file handler: ETag, Last-Modified, byte ranges, MIME sniff.

## Public items

| Name | Kind | Description |
|---|---|---|
| `FileServer` | type | Static-file handler rooted at a directory (Rust-side; streaming). |
| `serve_file` | fn | Read a single file and return it as a Response struct. Interp tier. |
| `mime_for_path` | fn | Guess a MIME type from a file path's extension. Available in interp + compiled. |

