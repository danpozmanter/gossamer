# `std::http::session`

Status: shipped

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

