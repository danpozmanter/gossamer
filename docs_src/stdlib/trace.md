# `std::trace`

Status: shipped

W3C trace-context-compatible distributed tracing. Identifier types, request-scoped SpanContext, process-level Tracer, and OTLP JSON export.

## Public items

| Name | Kind | Description |
|---|---|---|
| `TraceId` | type | 128-bit trace identifier (W3C trace-context format). |
| `SpanId` | type | 64-bit span identifier. |
| `SpanContext` | type | Request-scoped trace + span pair, propagated through `std::context`. |
| `SpanStatus` | type | Span outcome: Unset / Ok / Error(message). |
| `Span` | type | Active span builder. Attributes, events, status; `end()` finalises and records. |
| `EndedSpan` | type | Finalised span record; `to_otlp_json()` serialises for OTLP/HTTP export. |
| `Tracer` | type | Process-level span sink. `start_span`, `ended_spans`, `set_global`. |
| `SpanGuard` | type | RAII guard returned by `enter_span`; restores the prior active span on drop. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/trace.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`EndedSpan`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/trace.rs) | `type` — see the source declaration | Finalised span record; `to_otlp_json()` serialises for OTLP/HTTP export. |
| [`Span`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/trace.rs) | `type` — see the source declaration | Active span builder. Attributes, events, status; `end()` finalises and records. |
| [`SpanContext`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/trace.rs) | `type` — see the source declaration | Request-scoped trace + span pair, propagated through `std::context`. |
| [`SpanGuard`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/trace.rs) | `type` — see the source declaration | RAII guard returned by `enter_span`; restores the prior active span on drop. |
| [`SpanId`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/trace.rs) | `type` — see the source declaration | 64-bit span identifier. |
| [`SpanStatus`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/trace.rs) | `type` — see the source declaration | Span outcome: Unset / Ok / Error(message). |
| [`TraceId`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/trace.rs) | `type` — see the source declaration | 128-bit trace identifier (W3C trace-context format). |
| [`Tracer`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/trace.rs) | `type` — see the source declaration | Process-level span sink. `start_span`, `ended_spans`, `set_global`. |
