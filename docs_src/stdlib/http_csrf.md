# `std::http::csrf`

Status: experimental

Double-submit-cookie CSRF protection with Origin / Referer allowlist.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_csrf.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Config`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_csrf.rs) | `type Config` | Signing key, cookie / header names, and origin allowlist. |
| [`RouteAuth`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_csrf.rs) | `type RouteAuth` | Per-route policy: Required, Optional, or Skipped. |
| [`attach_cookie`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_csrf.rs) | `fn attach_cookie(request: http::Request, secret: String) -> Result<http::Request, errors::Error>` | Set the CSRF cookie on a Response. |
| [`check`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_csrf.rs) | `fn check(request: http::Request, secret: String) -> Result<http::Request, errors::Error>` | Combined origin + token gate; returns Err on failure. |
| [`extract_token`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_csrf.rs) | `fn extract_token(request: http::Request, secret: String) -> Result<http::Request, errors::Error>` | Pull a token from the configured header or form field. |
| [`issue_token`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_csrf.rs) | `fn issue_token(secret: Vec<u8>) -> Result<String, errors::Error>` | Mint a fresh CSRF token bound to the configured signing key. |
| [`origin_allowed`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_csrf.rs) | `fn origin_allowed(request: http::Request, secret: String) -> Result<http::Request, errors::Error>` | Origin / Referer allowlist check for unsafe methods. |
| [`verify_token`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_csrf.rs) | `fn verify_token(cookie_token: String, supplied_token: String, secret: Vec<u8>) -> Result<(), errors::Error>` | Constant-time verify of a presented token against the cookie value. |
