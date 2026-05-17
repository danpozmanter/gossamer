# `std::path::native`

Status: shipped

Native-separator wrappers over `std::path` (backslash on Windows).

## Public items

| Name | Kind | Description |
|---|---|---|
| `SEPARATOR` | const | Platform-preferred path separator character. |
| `join` | fn | Joins two components using the platform separator. |
| `clean` | fn | Canonicalises a path into native-separator form. |
| `to_posix` | fn | Rewrites a native-separator path into posix form. |
| `to_native` | fn | Rewrites a posix path into native-separator form. |

