# `std::http`

HTTP/1.1 client and server.

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

