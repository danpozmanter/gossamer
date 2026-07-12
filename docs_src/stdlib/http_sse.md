# `std::http::sse`

Status: shipped

Server-Sent Events (text/event-stream) emitter with heartbeat ticks and retry hint.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Stream` | type | Active SSE stream - handler writes events through it (Rust-side). |
| `Event` | type | One SSE event (id, event, data, retry). |
| `encode_event` | fn | Render one event block as a string: `(event, data, id) -> String`. Available in interp + compiled. |
| `encode_comment` | fn | Render a `:`-prefixed keepalive line. Available in interp + compiled. |
| `encode_retry` | fn | Render a `retry:` reconnect-hint directive in milliseconds. Available in interp + compiled. |

