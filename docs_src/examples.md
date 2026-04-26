# Examples

The [`examples/`](https://github.com/danpozmanter/gossamer/tree/main/examples)
directory ships a handful of worked programs.

## A friendly taste

`examples/function_piping.gos` walks through the `|>` forward-pipe
operator, the feature most likely to surprise readers coming from
Rust or Go. It straightens out nested calls so the data flow reads
left-to-right:

```gossamer
fn double(x: i64) -> i64 { x * 2 }
fn add(a: i64, b: i64) -> i64 { a + b }
fn clamp(lo: i64, hi: i64, x: i64) -> i64 {
    if x < lo { lo } else if x > hi { hi } else { x }
}

fn main() {
    let n = 3i64 |> double |> add(10i64) |> clamp(0i64, 100i64)
    println("arithmetic:", n)
}
```

## Running today

- **`hello_world.gos`** — one-liner that prints via `fmt::println`.
  Runs under `gos run`.
- **`function_piping.gos`** — tour of the `|>` forward-pipe
  operator, both arithmetic chains and method pipelines.
- **`go_spawn.gos`** — goroutine fan-out with no channels.
  Every construct lowers through native codegen, so `gos build`
  produces a working binary.
- **`concurrency.gos`** — goroutines plus a `(Sender, Receiver)`
  channel, producer / consumer shape. Runs under `gos run`
  (tree-walker and VM); native codegen for channel operations
  is still pending.
- **`line_count.gos`** — walks a directory via `os::read_dir`,
  counts plain-text lines per file, fans out through a channel.
  Uses goroutines and `select`.
- **`web_server.gos`** — HTTP/1.1 echo server mirroring FastAPI's
  `/echo` handler. Accepts any method, returns method / path /
  query / body as JSON. Runs under `gos run`; `curl
  http://localhost:8080/echo?name=jane` exercises it.

## Parse-only today (run once the stdlib wiring lands)

- **`kv_cache.gos`** — in-memory TTL cache with a background
  expiry sweeper. Exercises goroutines, `Mutex<T>`, channels,
  graceful shutdown via `std::context`.
- **`json_pipeline.gos`** — streaming JSONL transformer. Reads
  line-delimited JSON from stdin, applies a transform, writes
  JSONL to stdout. Exercises `std::io`, `std::encoding::json`,
  `std::errors::wrap`.

## Self-host ports

`examples/selfhost/` holds parse-only ports of Gossamer's own
lexer and parser, as described in
[`docs/selfhosting.md`](design/selfhosting.md). These are a
feasibility study — they will *build* once the stdlib covers
growable collections, hashmaps, and generics through codegen.

## Try it

```sh
gos run examples/hello_world.gos
gos run examples/function_piping.gos
gos run examples/web_server.gos &
curl 'http://localhost:8080/echo?name=jane'
```
