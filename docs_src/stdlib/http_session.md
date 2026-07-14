# `std::http::session`

Status: experimental

Signed-cookie session store with pluggable backend trait.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Session` | type | Per-request session view; mutations persist on response. |
| `SessionConfig` | type | Cookie name, domain, signing key, serialization mode. |
| `SessionStore` | trait | Backend interface: load / save / delete by session id. |
| `SignedCookieStore` | type | Cookie-backed store with HMAC signature; no server state. |
| `SerializationMode` | type | Session payload encoding: Json or Bincode. |
| `with_session` | fn | Run a closure with the session bound; persist any mutations. |
| `sign` | fn | Sign session data into a tamper-evident cookie value. |
| `verify` | fn | Verify and decode a signed session cookie value. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_session.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`SerializationMode`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_session.rs) | `type` — see the source declaration | Session payload encoding: Json or Bincode. |
| [`Session`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_session.rs) | `type` — see the source declaration | Per-request session view; mutations persist on response. |
| [`SessionConfig`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_session.rs) | `type` — see the source declaration | Cookie name, domain, signing key, serialization mode. |
| [`SessionStore`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_session.rs) | `trait` — see the source declaration | Backend interface: load / save / delete by session id. |
| [`SignedCookieStore`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_session.rs) | `type` — see the source declaration | Cookie-backed store with HMAC signature; no server state. |
| [`sign`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_session.rs) | `fn sign(value: String, secret: Vec<u8>) -> String` | Sign session data into a tamper-evident cookie value. |
| [`verify`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_session.rs) | `fn verify(value: String, secret: Vec<u8>) -> Result<String, errors::Error>` | Verify and decode a signed session cookie value. |
| [`with_session`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_session.rs) | `fn with_session(request: http::Request, secret: String) -> Result<http::Request, errors::Error>` | Run a closure with the session bound; persist any mutations. |
