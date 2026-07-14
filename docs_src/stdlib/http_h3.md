# `std::http_h3`

Status: experimental

HTTP/3 over QUIC. std::http_h3 is the retained 0.27 spelling; no std::http::h3 alias.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Handler` | trait | Per-request handler. `fn serve(&self, request: Request) -> Response`. |
| `H3Error` | type | Transport / protocol error variants surfaced from quinn + h3. |
| `serve` | fn | Run an HTTP/3 server bound to `addr` with TLS certificate + key paths and the supplied handler. |
| `Client` | type | HTTP/3 client. `new` validates against the Mozilla root store; `insecure` skips verification (dev only). Methods: `get`, `post`, `put`, `delete`, `head`, `request`. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_h3.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Client`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_h3.rs) | `type Client` | HTTP/3 client. `new` validates against the Mozilla root store; `insecure` skips verification (dev only). Methods: `get`, `post`, `put`, `delete`, `head`, `request`. |
| [`H3Error`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_h3.rs) | `type H3Error` | Transport / protocol error variants surfaced from quinn + h3. |
| [`Handler`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_h3.rs) | `trait Handler` | Per-request handler. `fn serve(&self, request: Request) -> Response`. |
| [`serve`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_h3.rs) | `fn serve(addr: String, cert_path: String, key_path: String, handler: http_h3::Handler) -> Result<(), errors::Error>` | Run an HTTP/3 server bound to `addr` with TLS certificate + key paths and the supplied handler. |
