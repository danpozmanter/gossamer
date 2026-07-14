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

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_websocket.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Error`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_websocket.rs) | `type` — see the source declaration | Io / Protocol / BadHandshake. |
| [`Message`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_websocket.rs) | `type` — see the source declaration | Text / Binary / Ping / Pong / Close. |
| [`WebSocket`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_websocket.rs) | `type` — see the source declaration | Accepted WebSocket connection (Rust-side framing). |
| [`accept`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_websocket.rs) | `fn accept(request: http::Request) -> Result<http::websocket::Conn, errors::Error>` | Upgrade an incoming Request to a WebSocket (Rust-side). |
| [`accept_key`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_websocket.rs) | `fn accept_key(key: String) -> String` | Compute RFC 6455 Sec-WebSocket-Accept from a client nonce. Available in interp + compiled. |
| [`close`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_websocket.rs) | `fn close(conn: http::websocket::Conn) -> Result<(), errors::Error>` | close(ws) -> Result<(), Error>: send a close frame and release the handle. |
| [`connect`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_websocket.rs) | `fn connect(url: String) -> Result<http::websocket::Conn, errors::Error>` | connect(url) -> Result<i64, Error>: client TCP connect + RFC 6455 upgrade; returns a WebSocket handle. |
| [`is_websocket_upgrade`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_websocket.rs) | `fn is_websocket_upgrade(request: http::Request) -> bool` | Test whether an incoming Request carries a WebSocket upgrade handshake. Interp tier. |
| [`recv`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_websocket.rs) | `fn recv(conn: http::websocket::Conn) -> Result<http::websocket::Message, errors::Error>` | recv(ws) -> Result<String, Error>: next text message; Err on close/error. |
| [`send_binary`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_websocket.rs) | `fn send_binary(conn: http::websocket::Conn, data: Vec<u8>) -> Result<(), errors::Error>` | send_binary(ws, data) -> Result<(), Error>: send one binary frame. |
| [`send_text`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_websocket.rs) | `fn send_text(conn: http::websocket::Conn, text: String) -> Result<(), errors::Error>` | send_text(ws, s) -> Result<(), Error>: send one text frame. |
| [`serve`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_websocket.rs) | `fn serve(addr: String, handler: Fn(http::websocket::Conn) -> ()) -> Result<(), errors::Error>` | serve(addr, handler) -> Result<(), Error>: bind, upgrade each connection, dispatch the handler's handle(&self, ws) per connection. |
