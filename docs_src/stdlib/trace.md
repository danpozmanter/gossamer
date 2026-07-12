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

