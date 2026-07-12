# `std::metrics`

Status: experimental

Prometheus-compatible primitives (Counter, Gauge, Histogram) and a Registry rendering the standard text-exposition format.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Counter` | type | Monotonic-increasing u64 counter (lock-free). |
| `Gauge` | type | Set / inc / dec gauge (lock-free). |
| `Histogram` | type | Bucketed observation histogram with sum and count. |
| `Metric` | type | Enum holding any of the three primitives for registry storage. |
| `Registry` | type | Ordered collection of metrics; `render()` emits the Prometheus text-exposition format. |
| `serve_metrics` | fn | Mounts a registry on `/metrics` over the existing http server loop. |

