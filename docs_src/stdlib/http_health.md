# `std::http::health`

Status: experimental

Liveness / readiness probes for HTTP health endpoints.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Probe` | trait | One health check returning Ok or Err with a short message. |
| `Health` | type | Aggregates a set of named probes into a single status. |

