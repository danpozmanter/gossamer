# `std::tls`

TLS termination and TLS client dialling (rustls-backed). Wired through both http::Server::bind_and_run_tls and http::Client; mTLS / ALPN / SNI exposed.

## Public items

| Name | Kind | Description |
|---|---|---|
| `CertKey` | type | PEM-encoded certificate chain + private key. |
| `ServerConfig` | type | Opaque server-side TLS configuration. |
| `ClientConfig` | type | Opaque client-side TLS configuration. |
| `server_config` | fn | Builds a server config from a CertKey. Returns Err until rustls lands. |
| `client_config` | fn | Builds a client config. Returns Err until rustls lands. |

