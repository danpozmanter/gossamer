# `std::trace`

Status: experimental

W3C trace-context-compatible distributed tracing. Identifier types, request-scoped SpanContext, process-level Tracer, and OTLP JSON export.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/trace.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`EndedSpan`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/trace.rs) | `type EndedSpan` | Finalised span record; `to_otlp_json()` serialises for OTLP/HTTP export. |
| [`Span`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/trace.rs) | `type Span` | Active span builder. Attributes, events, status; `end()` finalises and records. |
| [`SpanContext`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/trace.rs) | `type SpanContext` | Request-scoped trace + span pair, propagated through `std::context`. |
| [`SpanGuard`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/trace.rs) | `type SpanGuard` | RAII guard returned by `enter_span`; restores the prior active span on drop. |
| [`SpanId`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/trace.rs) | `type SpanId` | 64-bit span identifier. |
| [`SpanStatus`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/trace.rs) | `type SpanStatus` | Span outcome: Unset / Ok / Error(message). |
| [`TraceId`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/trace.rs) | `type TraceId` | 128-bit trace identifier (W3C trace-context format). |
| [`Tracer`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/trace.rs) | `type Tracer` | Process-level span sink. `start_span`, `ended_spans`, `set_global`. |
