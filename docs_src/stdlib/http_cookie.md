# `std::http::cookie`

Status: experimental

RFC 6265 cookie parser and Set-Cookie builder.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Cookie` | type | Parsed cookie with name, value, and Set-Cookie attributes. |
| `CookieBuilder` | type | Fluent builder for Set-Cookie response headers. |
| `SameSite` | type | SameSite attribute: Strict / Lax / None. |
| `parse_cookie_header` | fn | Parse a Cookie request header into (name, value) pairs. |
| `serialize` | fn | Render a Cookie as a Set-Cookie header value. |

