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

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/metrics.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Counter`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/metrics.rs) | `type` — see the source declaration | Monotonic-increasing u64 counter (lock-free). |
| [`Gauge`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/metrics.rs) | `type` — see the source declaration | Set / inc / dec gauge (lock-free). |
| [`Histogram`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/metrics.rs) | `type` — see the source declaration | Bucketed observation histogram with sum and count. |
| [`Metric`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/metrics.rs) | `type` — see the source declaration | Enum holding any of the three primitives for registry storage. |
| [`Registry`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/metrics.rs) | `type` — see the source declaration | Ordered collection of metrics; `render()` emits the Prometheus text-exposition format. |
| [`serve_metrics`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/metrics.rs) | `fn serve_metrics(addr: String) -> Result<(), errors::Error>` | Mounts a registry on `/metrics` over the existing http server loop. |
