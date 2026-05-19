# `std::http_h3`

Status: shipped

First-party HTTP/3 server + client over QUIC (RFC 9114; quinn + h3). Each `serve` and `Client` instance owns a private tokio runtime; callers see only synchronous entry points.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Handler` | trait | Per-request handler. `fn serve(&self, request: Request) -> Response`. |
| `H3Error` | type | Transport / protocol error variants surfaced from quinn + h3. |
| `serve` | fn | Run an HTTP/3 server bound to `addr` with TLS certificate + key paths and the supplied handler. |
| `Client` | type | HTTP/3 client. `new` validates against the Mozilla root store; `insecure` skips verification (dev only). Methods: `get`, `post`, `put`, `delete`, `head`, `request`. |

