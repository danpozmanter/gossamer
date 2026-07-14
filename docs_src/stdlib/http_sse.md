# `std::http::sse`

Status: experimental

Server-Sent Events (text/event-stream) emitter with heartbeat ticks and retry hint.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_sse.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Event`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_sse.rs) | `type Event` | One SSE event (id, event, data, retry). |
| [`Stream`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_sse.rs) | `type Stream` | Active SSE stream - handler writes events through it (Rust-side). |
| [`encode_comment`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_sse.rs) | `fn encode_comment(comment: String) -> Vec<u8>` | Render a `:`-prefixed keepalive line. Available in interp + compiled. |
| [`encode_event`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_sse.rs) | `fn encode_event(event: String, data: String, id: String) -> Vec<u8>` | Render one event block as a string: `(event, data, id) -> String`. Available in interp + compiled. |
| [`encode_retry`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_sse.rs) | `fn encode_retry(ms: i64) -> Vec<u8>` | Render a `retry:` reconnect-hint directive in milliseconds. Available in interp + compiled. |
