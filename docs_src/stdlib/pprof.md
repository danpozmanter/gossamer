# `std::pprof`

Status: experimental

Runtime profiles in the text format `go tool pprof` reads, plus a Chrome-trace scheduler capture.


<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-runtime/src/pprof.rs) lives in the runtime rather than the standard library, so the bytecode VM and the compiled tiers render from one implementation over one set of scheduler counters.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`goroutine_profile`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-runtime/src/pprof.rs) | `fn goroutine_profile() -> String` | Text profile with one sample per live goroutine and its last-known frame. |
| [`mutex_profile`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-runtime/src/pprof.rs) | `fn mutex_profile() -> String` | Text profile of microseconds parked on synchronization since process start. |
| [`block_profile`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-runtime/src/pprof.rs) | `fn block_profile() -> String` | Text profile of microseconds parked on channels, I/O, and timers since process start. |
| [`execution_trace`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-runtime/src/pprof.rs) | `fn execution_trace(millis: i64) -> String` | Chrome trace JSON of scheduler spawn/park/unpark events; blocks for the given window. |
| [`route`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-runtime/src/pprof.rs) | `fn route(path: String, query: String) -> Option<String>` | Serves a `/debug/pprof/...` path, returning the body to write, or `None` for an unknown path. |

## Formats

The three profiles render the legacy text shape `go tool pprof -text` reads:

```text
# pprof text format v1
samples=N self=N
  function file:line
```

`execution_trace` returns a Chrome trace object (`{"traceEvents":[...]}`) that `chrome://tracing` and Perfetto load directly.

## Mounting the endpoints

`route` answers the paths Go's `net/http/pprof` uses, so a handler can forward straight to it:

```text
use std::http
use std::pprof

fn debug_handler(r: http::Request) -> Result<http::Response, errors::Error> {
    match pprof::route(r.path, r.query) {
        Some(body) => Ok(http::Response::text(200, body))
        None => Ok(http::Response::text(404, "not found"))
    }
}
```

Paths served: `/debug/pprof/` (index), `/debug/pprof/goroutine`, `/debug/pprof/mutex`, `/debug/pprof/block`, and `/debug/pprof/trace?seconds=N`.

## Sampling

CPU and heap profiles need a sampler feeding the profile buffers; there is none yet, so those two shapes are absent rather than returning an empty profile.
