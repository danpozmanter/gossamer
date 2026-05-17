# `std::http::websocket`

Status: shipped

RFC 6455 WebSocket support. Server-side accept + send_text / send_binary / ping / pong / close.

## Public items

| Name | Kind | Description |
|---|---|---|
| `WebSocket` | type | Accepted WebSocket connection (Rust-side framing). |
| `Message` | type | Text / Binary / Ping / Pong / Close. |
| `accept` | fn | Upgrade an incoming Request to a WebSocket (Rust-side). |
| `Error` | type | Io / Protocol / BadHandshake. |
| `accept_key` | fn | Compute RFC 6455 Sec-WebSocket-Accept from a client nonce. Available in interp + compiled. |
| `is_websocket_upgrade` | fn | Test whether an incoming Request carries a WebSocket upgrade handshake. Interp tier. |

