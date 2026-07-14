# `std::http::request_streaming`

Status: experimental

HTTP/2 request-body streaming is currently a Rust-side server scaffold, not a
stable Gossamer handler ABI.

The runtime can drive a bounded, flow-control-aware
`RequestStreamingHandler` for HTTP/2 request bodies. That path supports
incremental chunk reads, trailers, stream deadlines, and receive-capacity
release.

Gossamer handlers still receive a complete bounded `http::Request` on the VM
and AOT tiers. Use `Request.raw_body` for bounded uploads until the public
streaming handler ABI is exposed across tiers.

See [`std::http`](http.md#http2-request-streaming) for the HTTP server context
and current request-body limits.
