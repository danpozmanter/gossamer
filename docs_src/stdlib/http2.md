# `std::http2`

HTTP/2 server (h2 crate over goroutine future-driver). Bounded and streaming handler shapes; ALPN-aware HTTPS dispatch via http::server::bind_and_run_tls_h2.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Handler` | trait | Bounded-body handler: serve(Request) -> Response. |
| `StreamingHandler` | trait | Chunked-body handler: serve(Request, ResponseWriter) -> Result<(), Error>. |
| `ResponseWriter` | type | Streaming response writer; set_status / header / write_chunk / finish. |
| `Config` | type | Per-connection h2 tuning (window sizes, max concurrent streams, frame caps). |
| `ServerHandle` | type | Handle returned by serve_connection to inspect in-flight count and trigger graceful shutdown. |
| `Error` | type | h2 server error: Io, Protocol, Handler. |
| `serve_connection` | fn | Drive an HTTP/2 connection on the calling goroutine (bounded Handler). |
| `serve_connection_streaming` | fn | Same shape for StreamingHandler. |
| `bind_and_run_h2c` | fn | Bind a plain-TCP listener and serve h2c (HTTP/2 cleartext). |
| `bind_and_run_h2c_streaming` | fn | Same shape for StreamingHandler. |

