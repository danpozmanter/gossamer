# Examples

The [`examples/`](https://github.com/danpozmanter/gossamer/tree/main/examples)
directory ships a handful of worked programs.

## A friendly taste

`examples/function_piping.gos` walks through the `|>` forward-pipe
operator and the F#-style combinator surface in `std::iter` /
`std::option` (SPEC §10.4 / §10.4a). `|>` straightens out nested
calls so the data flow reads left-to-right; the combinators take
the data value as the last positional parameter so each call
threads naturally:

```gossamer
use std::iter
use std::option

fn double(x: i64) -> i64 { x * 2 }
fn add(a: i64, b: i64) -> i64 { a + b }
fn clamp(lo: i64, hi: i64, x: i64) -> i64 {
    if x < lo { lo } else if x > hi { hi } else { x }
}

fn main() {
    let n = 3 |> double |> add(10) |> clamp(0, 100)
    println!("arithmetic: {n}")

    let total = iter::range_inclusive(1, 10)
        |> iter::filter(|n: i64| n % 2 == 0)
        |> iter::sum_by(|n: i64| n * n)
    println!("sum of even squares: {total}")

    let xs = [1, 3, 5, 9, 14, 21]
    let first_big = xs
        |> iter::find(|n: i64| n > 10)
        |> option::default(-1)
    println!("first > 10 (or -1): {first_big}")
}
```

## Running today

- **`hello_world.gos`** — one-liner that prints via `fmt::println`.
  Runs under `gos run`.
- **`function_piping.gos`** — tour of the `|>` forward-pipe
  operator plus the `std::iter` / `std::option` combinator
  surface (`filter`, `sum_by`, `find`, `option::default`, …).
  Runs under `gos run`, `gos build` (cranelift), and
  `gos build --release` (LLVM); the tier_parity test confirms
  identical output across all three.
- **`generic_struct.gos`** — three generic struct shapes: `Pair<A, B>`
  (two independent parameters), `SameType<T>` (one parameter shared by
  both fields, enabling field arithmetic), and `Triple<A, B, C>` (three
  parameters). Each construction site is a separate monomorphisation;
  parameters are inferred from the field values at the call site.
  Runs under `gos run` and `gos build`.
- **`go_spawn.gos`** — goroutine fan-out with no channels.
  Every construct lowers through native codegen, so `gos build`
  produces a working binary.
- **`concurrency.gos`** — goroutines plus a `(Sender, Receiver)`
  channel, producer / consumer shape. Runs under `gos run`
  (bytecode VM) and `gos build` (native), with channel operations
  lowered natively on every tier.
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
