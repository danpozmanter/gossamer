# `std::http::health`

Status: shipped

Liveness / readiness probes for HTTP health endpoints.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Probe` | trait | One health check returning Ok or Err with a short message. |
| `Health` | type | Aggregates a set of named probes into a single status. |
| `always_ok` | fn | Probe that always reports healthy. |
| `always_fail` | fn | Probe that always reports unhealthy with the given message. |
| `tcp_probe` | fn | Probe that opens a TCP connection within a deadline. |

