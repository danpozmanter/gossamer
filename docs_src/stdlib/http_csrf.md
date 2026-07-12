# `std::http::csrf`

Status: experimental

Double-submit-cookie CSRF protection with Origin / Referer allowlist.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Config` | type | Signing key, cookie / header names, and origin allowlist. |
| `RouteAuth` | type | Per-route policy: Required, Optional, or Skipped. |
| `issue_token` | fn | Mint a fresh CSRF token bound to the configured signing key. |
| `verify_token` | fn | Constant-time verify of a presented token against the cookie value. |
| `extract_token` | fn | Pull a token from the configured header or form field. |
| `origin_allowed` | fn | Origin / Referer allowlist check for unsafe methods. |
| `check` | fn | Combined origin + token gate; returns Err on failure. |
| `attach_cookie` | fn | Set the CSRF cookie on a Response. |

