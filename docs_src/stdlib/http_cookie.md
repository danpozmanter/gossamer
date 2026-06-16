# `std::http::cookie`

Status: shipped

RFC 6265 cookie parser and Set-Cookie builder.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Cookie` | type | Parsed cookie with name, value, and Set-Cookie attributes. |
| `CookieBuilder` | type | Fluent builder for Set-Cookie response headers. |
| `SameSite` | type | SameSite attribute: Strict / Lax / None. |
| `parse_cookie_header` | fn | Parse a Cookie request header into (name, value) pairs. |
| `parse_set_cookie` | fn | Parse a Set-Cookie response header into a Cookie. |
| `serialize` | fn | Render a Cookie as a Set-Cookie header value. |

