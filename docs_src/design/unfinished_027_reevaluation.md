# Re-evaluation Of Unfinished 0.27 Items

This note narrows the remaining 0.27 umbrellas into separately testable
follow-ups. These are not release-exit claims; they record what stays in scope,
what is already covered, and what must remain Experimental or split until
specific evidence lands.

| Item | Decision | Current evidence | Follow-up boundary |
|---|---|---|---|
| HTTP/2 public request streaming | Keep, narrow and keep Experimental | HTTP/1.1 + HTTP/2 server/client, response streaming, push, trailers, limits, deadlines, and the Rust-side `RequestStreamingHandler` scaffold are covered or documented. `std::http::request_streaming` records that public Gossamer handlers still receive bounded complete `Request` values on VM and AOT. | Add the public VM/AOT streaming handler ABI plus cancellation/disconnect, trailer, slowloris, reset, timeout, and handler-abandonment parity before claiming request streaming as shipped. |
| HTTP/3 public request/response streaming | Keep, narrow and keep Experimental | `std::http_h3` resolves, is catalogued, and the feature registry explicitly says public bodies are fully buffered while enforcing connection, stream, header, body, QUIC window, idle-timeout, and per-stream I/O limits. | Ship streaming bodies/backpressure only when the public H3 contract matches H2 where QUIC permits; do not mark Shipped while buffering is the user-visible contract. |
| Socket-to-file package ingestion | Keep, narrow | Fetch uses reader/writer transport, bounded hash spool, owner-only temp files on Unix, reader-based Ed25519 verification, validated extraction, and atomic cache publish tests. Focused unit tests cover the private spool mode and byte cap. | Remove remaining map-shaped source compatibility from the cache path and keep compressed/expanded/file-count/path/symlink limits as the acceptance boundary. |
| General collection planning / ownership / bounds | Keep, split | Benchmark-relevant push-loop reserve, local allocation planning, move-into-container ownership, dominator bounds facts, Vec ABI capacity, HashMap key behavior, JSON number preservation, and string capacity work are closed in Sections 2 and 3. | Track broader JSON streaming and collection ownership as separate, evidence-backed items rather than one permanent umbrella. |
