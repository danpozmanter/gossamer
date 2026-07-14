# `std::http::health`

Status: experimental

Liveness / readiness probes for HTTP health endpoints.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Probe` | trait | One health check returning Ok or Err with a short message. |
| `Health` | type | Aggregates a set of named probes into a single status. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_health.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Health`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_health.rs) | `type` — see the source declaration | Aggregates a set of named probes into a single status. |
| [`Probe`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_health.rs) | `trait` — see the source declaration | One health check returning Ok or Err with a short message. |
