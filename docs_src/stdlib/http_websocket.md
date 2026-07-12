# `std::http::websocket`

Status: experimental

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
| `serve` | fn | serve(addr, handler) -> Result<(), Error>: bind, upgrade each connection, dispatch the handler's handle(&self, ws) per connection. |
| `connect` | fn | connect(url) -> Result<i64, Error>: client TCP connect + RFC 6455 upgrade; returns a WebSocket handle. |
| `send_text` | fn | send_text(ws, s) -> Result<(), Error>: send one text frame. |
| `send_binary` | fn | send_binary(ws, data) -> Result<(), Error>: send one binary frame. |
| `recv` | fn | recv(ws) -> Result<String, Error>: next text message; Err on close/error. |
| `close` | fn | close(ws) -> Result<(), Error>: send a close frame and release the handle. |

