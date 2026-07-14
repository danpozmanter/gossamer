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

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_cookie.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Cookie`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_cookie.rs) | `type` — see the source declaration | Parsed cookie with name, value, and Set-Cookie attributes. |
| [`CookieBuilder`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_cookie.rs) | `type` — see the source declaration | Fluent builder for Set-Cookie response headers. |
| [`SameSite`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_cookie.rs) | `type` — see the source declaration | SameSite attribute: Strict / Lax / None. |
| [`parse_cookie_header`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_cookie.rs) | `fn parse_cookie_header(header: String) -> Vec<http::cookie::Cookie>` | Parse a Cookie request header into (name, value) pairs. |
| [`serialize`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_cookie.rs) | `fn serialize(name: String, value: String) -> String` | Render a Cookie as a Set-Cookie header value. |
