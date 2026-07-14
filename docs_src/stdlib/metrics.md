# `std::metrics`

Status: experimental

Prometheus-compatible primitives (Counter, Gauge, Histogram) and a Registry rendering the standard text-exposition format.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/metrics.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Counter`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/metrics.rs) | `type Counter` | Monotonic-increasing u64 counter (lock-free). |
| [`Gauge`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/metrics.rs) | `type Gauge` | Set / inc / dec gauge (lock-free). |
| [`Histogram`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/metrics.rs) | `type Histogram` | Bucketed observation histogram with sum and count. |
| [`Metric`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/metrics.rs) | `type Metric` | Enum holding any of the three primitives for registry storage. |
| [`Registry`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/metrics.rs) | `type Registry` | Ordered collection of metrics; `render()` emits the Prometheus text-exposition format. |
| [`serve_metrics`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/metrics.rs) | `fn serve_metrics(addr: String) -> Result<(), errors::Error>` | Mounts a registry on `/metrics` over the existing http server loop. |
