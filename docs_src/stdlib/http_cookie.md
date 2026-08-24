# `std::http::cookie`

Status: experimental

RFC 6265 cookie parser and Set-Cookie builder.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/http_cookie.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Cookie`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/http_cookie.rs) | `type Cookie` | Parsed cookie with name, value, and Set-Cookie attributes. |
| [`CookieBuilder`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/http_cookie.rs) | `type CookieBuilder` | Fluent builder for Set-Cookie response headers. |
| [`SameSite`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/http_cookie.rs) | `type SameSite` | SameSite attribute: Strict / Lax / None. |
| [`parse_cookie_header`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/http_cookie.rs) | `fn parse_cookie_header(header: String) -> Vec<http::cookie::Cookie>` | Parse a Cookie request header into (name, value) pairs. |
| [`serialize`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/http_cookie.rs) | `fn serialize(name: String, value: String) -> String` | Render a Cookie as a Set-Cookie header value. |
