# `std::path`

Status: shipped

POSIX-style path manipulation.

## Public items

| Name | Kind | Description |
|---|---|---|
| `join` | fn | Joins two path fragments. |
| `split` | fn | Returns (dir, file) for the supplied path. |
| `parent` | fn | Parent directory, or None at the root. |
| `file_name` | fn | Final path component, or None. |
| `file_stem` | fn | File name without its extension. |
| `extension` | fn | Dotted extension as an Option. |
| `is_absolute` | fn | Reports whether the path is absolute. |
| `normalize` | fn | Lexically normalizes the path. |
| `starts_with` | fn | Reports whether the path begins with a prefix component-wise. |

