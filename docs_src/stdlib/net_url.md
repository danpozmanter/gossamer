# `std::net::url`

Status: shipped

Network URL parsing and component escaping; never use filesystem-path rules.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Url` | type | Parsed URL. |
| `query_escape` | fn | Percent-encodes a query parameter. |
| `query_unescape` | fn | Inverse of `query_escape`. |
| `path_escape` | fn | Percent-encodes a URL path segment. |
| `path_unescape` | fn | Inverse of `path_escape`. |

