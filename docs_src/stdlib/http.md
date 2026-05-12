# `std::http`

HTTP/1.1 and HTTP/2 client and server. HTTP/2 negotiates via ALPN over TLS automatically (Go-style); h2c entry points are explicit.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Request` | type | HTTP request value passed to a handler. |
| `Response` | type | HTTP response value returned from a handler. |
| `Method` | type | HTTP method enumeration. |
| `StatusCode` | type | HTTP status code. |
| `Headers` | type | Case-insensitive header map. |
| `Server` | type | HTTP server bound to a TCP listener. |
| `serve` | fn | Convenience: bind and serve an HTTP handler. |
| `Client` | type | HTTP client capable of GET/POST/PUT/DELETE. |
| `Http2Handler` | trait | Bounded-body HTTP/2 handler: serve(Request) -> Response. |
| `Http2StreamingHandler` | trait | Chunked-body HTTP/2 handler: serve(Request, StreamingResponseWriter) -> Result. |
| `StreamingResponseWriter` | type | Streaming HTTP/2 response writer; set_status / header / write_chunk / finish. |
| `Http2Config` | type | Per-connection HTTP/2 tuning (window sizes, max concurrent streams, frame caps). |
| `Http2ServerHandle` | type | Handle to a running HTTP/2 connection for shutdown / in-flight counts. |
| `Http2Error` | type | HTTP/2 server error: Io, Protocol, Handler. |
| `serve_h2_connection` | fn | Drive an HTTP/2 connection on the calling goroutine (bounded handler). |
| `serve_h2_connection_streaming` | fn | Same shape for Http2StreamingHandler. |
| `serve_h2c` | fn | Bind a plain-TCP listener and serve h2c (HTTP/2 cleartext). |
| `serve_h2c_streaming` | fn | Same shape for Http2StreamingHandler. |

