# WebAssembly and the browser playground

Gossamer runs **in the browser**. The bytecode VM is compiled to
`wasm32-unknown-unknown` and executes your code entirely client-side - no
server round-trip, no install. It powers the runnable examples on the
[home page](https://danpozmanter.github.io/gossamer/), the
[Tour of Gossamer](https://danpozmanter.github.io/gossamer/tour/), and the standalone
Try Gossamer editor.

Output is bit-identical to native `gos` / `gos build`.

## What runs

The whole language plus the pure standard library work unchanged:

- **Language** - types, pattern matching, generics, traits, closures,
  `Result` / `?`, the `|>` pipe, `arena { }`, deterministic reference counting.
- **Collections** - `Vec`, `HashMap`, `HashSet`, `BTreeMap`.
- **Pure stdlib** - `math`, `encoding` (json / yaml / toml / base64 / hex /
  binary / csv / pem / xml / base32), hashing (`crc32` / `adler32` / `fnv`),
  `regex`, iterators, `format!` / `println!`, `strings`, `unicode` / `utf8`,
  IP-address parsing, query/form parsing, HTML escaping, `gzip` / `flate` /
  `zlib`, and non-blocking `time`.

## Goroutines in the browser

A browser tab is single-threaded, so goroutines run **cooperatively and to
completion**:

- `spawn(f)` + `handle.join()` works - the body runs, you collect the result.
- A producer that fills a channel and closes it, drained by
  `while let Some(v) = rx.recv()`, works.
- Programs that need two goroutines to **interleave mid-run** (unbuffered
  rendezvous, ping-pong, `select` over live producers) do not - run those with
  native `gos`.

This is the same limit every language has on single-threaded WebAssembly.
Gossamer reports it as a clean panic rather than freezing the tab.

## What is omitted (the browser sandbox)

A browser tab cannot touch the filesystem, open sockets, or spawn processes, so
these are unavailable in the playground. They work normally in native `gos`
and `gos build`:

| Area | In the browser |
|------|----------------|
| Filesystem (`std::fs`) | unavailable |
| Sockets, TCP / UDP / TLS, HTTP server **and** client | unavailable |
| Processes, signals | unavailable |
| SQL (`std::database`) | unavailable |
| Crypto / JWT suite, metrics, tracing, `uuid` | unavailable |
| C-library codecs (`zstd`, `bzip2`) | unavailable (`gzip` / `flate` / `zlib` work) |

These are exactly the limits Go and Rust hit in the browser. The escape hatch
for any of them is a JavaScript bridge (`fetch`, `WebSocket`), which the
playground intentionally does not wire up.

## Compiling a program to a standalone `.wasm`

The playground compiles the **interpreter** to WebAssembly and runs your source
inside it. Compiling a Gossamer **program** to a standalone `.wasm` artifact -
`gos build --target wasm32-...` - is **not supported yet**: like every non-host
`--target`, it emits a placeholder artifact and reports `cross-link pending`
rather than a runnable binary. For now, "WebAssembly" means *run Gossamer in the
browser*, not *ship a `.wasm` binary*.
