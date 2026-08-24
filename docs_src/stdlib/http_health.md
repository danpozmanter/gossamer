# `std::http::health`

Status: experimental

Liveness and readiness endpoints are ordinary handlers over `std::lifecycle`: answer 200 from a liveness route, and 200/503 from `lifecycle::is_ready()` on a readiness route, which drops to false on its own when shutdown begins. A probe registry with per-check timeouts belongs in an application package.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/http_health.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Health`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/http_health.rs) | `type Health` | Aggregates a set of named probes into a single status. |
| [`Probe`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/http_health.rs) | `trait Probe` | One health check returning Ok or Err with a short message. |
