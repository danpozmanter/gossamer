# Migrating from Go to Gossamer

Gossamer takes goroutines, channels, defer, and the select
statement from Go. Most Go idioms map one-to-one. The biggest
source of friction is **syntax**: Gossamer is Rust-flavoured, so
`fn` not `func`, `let` not `:=`, `match` not `switch`, and so on.

This page walks the differences in three layers: syntax,
semantics, and stdlib equivalents. It is not exhaustive - for
the spec, see [`SPEC.md`](https://github.com/danpozmanter/gossamer/blob/main/SPEC.md).

## TL;DR

- **What stays the same:** goroutines, channels, defer,
  select, package-per-directory, structural concurrency,
  automatic memory management.
- **What changes:** syntax (Rust-shaped), error handling
  (`Result<T, E>` + `?`), interfaces become traits (nominal),
  no implicit numeric coercion (`as` is explicit), and memory is
  reclaimed deterministically - reference counting plus arenas,
  not a tracing GC, so no collector pauses.
- **What's missing today:** gRPC, real package registry,
  first-party SQL drivers. `std::database::sql` is the
  driver-pluggable surface (Conn / Tx / Stmt / Rows / Pool /
  migrate / Select); every driver - including SQLite - is a
  third-party Rust binding that registers itself at startup.
  HTTP/1, HTTP/2, HTTP/3, WebSockets, and SSE are all
  first-party (`std::http`, `std::http_h3`, `std::http::websocket`,
  `std::http::sse`).

## Syntax cheat-sheet

| Go | Gossamer | Notes |
|---|---|---|
| `func name(x int) int { return x + 1 }` | `fn name(x: i64) -> i64 { x + 1 }` | Trailing-expression-as-return; `return` keyword optional. |
| `var x int = 5` / `x := 5` | `let x: i64 = 5` / `let x = 5` | `let mut` for mutables. |
| `if x > 0 { … } else { … }` | `if x > 0 { … } else { … }` | Same. |
| `for i := 0; i < n; i++ { … }` | `for i in 0..n { … }` | Range-based for-loops. |
| `for { … }` | `loop { … }` | Infinite loop. |
| `for i, v := range xs { … }` | `for (i, v) in xs.iter().enumerate() { … }` | Iterator chain. |
| `switch x { case 1: … }` | `match x { 1 => …, _ => … }` | Pattern-matching is exhaustive - a missing arm is a compile error. |
| `type Point struct { X, Y int }` | `struct Point { x: i64, y: i64 }` | Lowercase fields by convention; visibility via `pub`. |
| `func (p Point) Norm() int { … }` | `impl Point { fn norm(&self) -> i64 { … } }` | Methods declared in `impl` block. |
| `type Reader interface { Read([]byte) int }` | `trait Reader { fn read(&self, buf: &mut [u8]) -> i64 }` | Traits are nominal - `impl Reader for MyType { … }`. |
| `var err error; if err != nil { … }` | `match call() { Ok(v) => …, Err(e) => … }` | `?` propagates `Err` automatically. |
| `defer cleanup()` | `defer cleanup()` | Same syntax, same semantics. |
| `go work()` | `go work()` | Same. |
| `ch <- v` / `v := <-ch` | `tx.send(v)` / `let v = rx.recv()` | Channels are typed: `channel::<i64>()`. |
| `select { case x := <-ch: … }` | `select { x = ch.recv() => …, default => … }` | Select arms are typed. |
| `make([]int, 0, 16)` | `Vec::<i64>::with_capacity(16)` | Vec API. |
| `make(map[string]int)` | `HashMap::<String, i64>::new()` | HashMap API. |
| `package main` + `func main() { … }` | bare statements at the top of the entry file | No `package main` and no `func main` are required: top-level statements become the body of an implicit `fn main()`. See [Top-level statements](../language/top_level_statements.md). |

## Semantic differences

### Errors

Go's convention is `(value, error)`. Gossamer uses `Result<T, E>`
with `?` propagation:

```go
// Go
data, err := os.ReadFile("config.toml")
if err != nil {
    return nil, fmt.Errorf("read config: %w", err)
}
```

```gos
// Gossamer
let data = fs::read("config.toml")?
```

`?` unwraps `Ok` or returns the `Err`. The function must declare
its return type as `Result<T, E>`.

For panicking on impossible errors, use `unwrap` or `expect`:

```gos
let data = fs::read("config.toml").expect("config required")
```

There is no nil-pointer equivalent. `Option<T>` is the
must-be-present-or-not type; matching is exhaustive.

### Interfaces vs. traits

Go interfaces are **structural**: any type with a matching method
set satisfies the interface implicitly.

Gossamer traits are **nominal**: you write `impl Trait for Type {
… }` to declare the conformance. The compiler does not infer it.

This means:

- A type can opt into multiple traits (same as Go).
- Two unrelated traits with identical method sets are *different*
  traits. Go has no concept of trait identity.
- Generic bounds use `T: Trait` syntax: `fn handle<T: Reader>(r: T)`,
  multi-bound `T: A + B`; each call site monomorphises statically.
- There are no trait objects (`dyn Trait`). For runtime polymorphism
  over a closed set, use an `enum` + exhaustive `match`.

### No implicit numeric coercion

Go silently widens `int32` to `int64`, etc. Gossamer requires
`as`:

```go
var x int32 = 5
var y int64 = int64(x)   // explicit in Go too, but...
var z int64 = x          // ...this also works in Go for untyped literals
```

```gos
let x: i32 = 5
let y: i64 = x as i64    // required
let z: i64 = x           // type error
```

This applies to every numeric width, signed↔unsigned, integer↔float.

### Visibility

Go uses lowercase-first-letter for unexported identifiers.
Gossamer uses an explicit `pub` keyword:

```gos
struct Config {
    pub name: String,    // exported
    secret: String,      // private to the module
}

pub fn parse_config(text: &str) -> Config { … }
```

The lowercase / case rule does *not* apply.

### `iota` and constants

Gossamer has no `iota`; use plain enums:

```go
type Severity int
const (
    Debug Severity = iota
    Info
    Warn
    Error
)
```

```gos
enum Severity {
    Debug,
    Info,
    Warn,
    Error,
}
```

C-style numeric variants are not yet supported in v1; the discriminants
are opaque to user code. If you need stable wire-format integers,
write the conversion table out by hand:

```gos
fn severity_to_int(s: Severity) -> i64 {
    match s {
        Severity::Debug => 0,
        Severity::Info => 1,
        Severity::Warn => 2,
        Severity::Error => 3,
    }
}
```

### Concurrency

Goroutine and channel syntax is the same. Behavioural notes:

- The M:N work-stealing scheduler is live; goroutines are parked by the
  netpoller when blocked on I/O.
- Scheduling uses watchdog-requested cooperative safepoints. Native compiled
  loops poll at amortized backedges and suspend the current coroutine. The VM
  yields its bounded OS worker pool at backedges but cannot suspend and resume a
  bytecode frame on a one-worker pool. This remains weaker than Go's fully
  asynchronous goroutine preemption. See
  [runtime design - Preemption](../design/runtime.md#preemption).
- Channels are created with `channel()` / `channel::<T>()`.
  `channel()` and `channel(0)` are unbuffered rendezvous channels,
  `channel(n)` for `n > 0` is bounded, and
  `channel::unbounded()` is the explicit queue form.
  The producer calls `close()` to end the stream, so
  `while let Some(v) = rx.recv()` is the canonical drain.
- `select` arms are typed by the channel they reference. Catch
  the unblocked case with `default =>`.

```gos
select {
    v = rx.recv() => println!("got: {}", v),
    tx.send(42) => println!("sent"),
    default => println!("would block"),
}
```

### Functional combinators replace per-loop accumulators

Go:

```go
total := 0
for _, n := range xs {
    if n%2 == 0 {
        total += n * n
    }
}
```

Gossamer:

```gos
let total = xs
    |> iter::filter(|n: i64| n % 2 == 0)
    |> iter::sum_by(|n: i64| n * n)
```

`std::iter` (SPEC §10.4) ships F#-style chaining combinators -
`map`, `filter`, `for_each`, `fold`, `sum_by`, `find`, `any`, `all`,
`take`, `skip`, `range`/`range_inclusive`, `chain`, `rev`,
plus closure-taking siblings. Argument order is data-last so each
combinator threads naturally through `|>`. Mirror modules for
`Option<T>` (`std::option`) and `Result<T, E>` (`std::result`)
cover the absent-or-default and error-mapping cases without a
`match` block.

The `for` loop stays in the language and stays idiomatic - for
side-effect loops with `break` / `continue`, for state machines
(running flag, accumulator pair, etc.), and for loops that need
to early-return. `iter::*` is for the *transformation* cases that
otherwise spawn a `let mut acc = 0; for x in xs { acc += … }`.

## Stdlib equivalents

| Go | Gossamer | Status |
|---|---|---|
| `fmt.Println` | `println!(...)` | ✓ |
| `fmt.Printf` | `println!("{x}")` interpolation | ✓ |
| `fmt.Sprintf` | `format!("{x}")` | ✓ |
| `os.Args` | `env::args()` | ✓ |
| `os.Getenv` | `env::var(name)` | ✓ |
| `os.Exit` | `process::exit(code)` | ✓ |
| `os.ReadFile` | `fs::read(path)` | ✓ |
| `os.WriteFile` | `fs::write(path, data)` | ✓ |
| `os/exec.Command` | `process::run(prog, &args)` | ✓ |
| `os/signal.Notify` | `os::signal::on(15)` | v1.x |
| `path/filepath.Walk` | `fs::walk_dir(root)` | v1.x |
| `bufio.NewScanner` | `bufio::Scanner::new(reader)` | v1.x |
| `compress/gzip` | `compress::gzip::Reader::new(r)` | v1.x |
| `encoding/json` | `json::encode(v)` / `json::decode::<T>(s)` | ✓ |
| `encoding/base64` | `encoding::base64::{encode,decode}` | ✓ |
| `encoding/hex` | `encoding::hex::{encode,decode}` | ✓ |
| `crypto/sha256` | `crypto::sha256::digest(input)` | ✓ |
| `crypto/hmac` | `crypto::hmac::sha256_mac(key, msg)` | ✓ |
| `crypto/rand` | `crypto::rand::bytes(n)` | ✓ |
| `crypto/subtle` | `crypto::subtle::constant_time_eq(a, b)` | ✓ |
| `net/http` server | `http::serve(addr, App {})` | ✓ (HTTP/1.1 only) |
| `net/http` client | `http::Client::new()` | ✓ |
| `net.Listen("tcp", …)` | `net::TcpListener::bind(addr)` | ✓ |
| `net/url.Parse` | `net::url::Url::parse(s)` | ✓ |
| `regexp.MustCompile` | `regex::compile(pattern).expect("…")` | ✓ |
| `sort.Slice` | `sort::sort_by(&mut xs, fn)` | ✓ |
| `strings.Split` | `strings::split(s, delim)` | ✓ |
| `strings.Replace` | `strings::replace(s, from, to)` | ✓ |
| `strings.TrimSpace` | `strings::trim(s)` | ✓ |
| `strconv.Atoi` | `strconv::parse_i64(s)` | ✓ |
| `strconv.Itoa` | `strconv::format_i64(n)` | ✓ |
| `time.Now` | `time::now()` | ✓ |
| `time.Sleep` | `time::sleep(d)` | ✓ |
| `time.Format` | `time::format(t, layout)` | v1.x |
| `time.Parse` | `time::parse(layout, s)` | v1.x |
| `flag.Parse` | `flag::parse()` | partial |
| `log/slog` | `slog::info(msg, key, value)` | ✓ |
| `context.Background` | `context::background()` | ✓ |
| `context.WithCancel` | `context::with_cancel(parent)` | ✓ |
| `sync.Mutex` | `sync::Mutex::new()` | ✓ |
| `sync.WaitGroup` | `sync::WaitGroup::new()` | ✓ |
| `sync.Once` | `sync::Once::new()` | ✓ |
| `sync/atomic` | `sync::AtomicI64::new(0)` | ✓ |

✓ = shipped in v1. *partial* = available but coverage is short of
Go's surface. *v1.x* = deferred. See
[`stdlib_coverage.md`](../stdlib_coverage.md) for the auto-generated
authoritative table.

## Translation worked examples

### Read a file, count lines

```go
// Go
data, err := os.ReadFile("input.txt")
if err != nil { return err }
n := strings.Count(string(data), "\n")
fmt.Println(n)
```

```gos
// Gossamer
let data = fs::read_to_string("input.txt")?
let n = strings::count(&data, "\n")
println!("{}", n)
```

### Spawn a worker, wait for it

```go
// Go
var wg sync.WaitGroup
wg.Add(1)
go func() {
    defer wg.Done()
    work()
}()
wg.Wait()
```

```gos
// Gossamer
let wg = sync::WaitGroup::new()
wg.add(1)
go fn() {
    defer wg.done()
    work()
}()
wg.wait()
```

### HTTP server

```go
// Go
http.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
    fmt.Fprintln(w, "hello")
})
log.Fatal(http.ListenAndServe(":8080", nil))
```

```gos
// Gossamer
struct App { }

impl http::Handler for App {
    fn serve(&self, r: http::Request) -> Result<http::Response, http::Error> {
        Ok(http::Response::text(200, "hello\n"))
    }
}

fn main() -> Result<(), http::Error> {
    http::serve("0.0.0.0:8080", App { })
}
```

### JSON encode / decode

```go
// Go
type User struct {
    Name string `json:"name"`
    Age  int    `json:"age"`
}
b, _ := json.Marshal(User{"Ada", 36})
var u User
_ = json.Unmarshal(b, &u)
```

```gos
// Gossamer
struct User {
    name: String,
    age: i64,
}

fn main() {
    let u = User { name: "Ada", age: 36 }
    let s = json::encode(&u).unwrap()
    let parsed: User = json::decode(&s).unwrap()
}
```

## Where Gossamer is harder than Go

Honest list:

- **Generic bounds are still narrow.** `fn f<T: Trait>(x: T)` does
  static, monomorphised dispatch - including generic structs that hold
  `T` by value (`struct Wrapper<T> { value: T }`, specialised per
  instantiation). What's missing is `dyn Trait`, operator-trait or
  associated-type bounds, and supertrait method inheritance through a
  bound. See [`codegen_abi.md`](../codegen_abi.md).
- **Tooling diagnostics are sometimes terser than Go's.** `gos
  explain CODE` exists but the corpus is younger.
- **No `go vet`-equivalent.** Lints exist (`gos lint`) but their
  surface is smaller.
- **No `go fmt` integration in editors out of the box.** Use the
  LSP server's format-on-save.

## Where Gossamer is easier than Go

Subjective list:

- Pattern matching with exhaustiveness checks catches the same
  class of bug Go's `switch`-without-default leaves to runtime.
- `Result<T, E>` + `?` is denser than `if err != nil { return … }`.
- Trait bounds let you write generic code without losing type
  information; Go's interfaces erase the concrete type.
- The `|>` pipe operator threads data through transformations
  more readably than `f(g(h(x)))`.

## Side-by-side recipes

### HTTP server

Go:

```go
http.HandleFunc("/notes", func(w http.ResponseWriter, r *http.Request) {
    fmt.Fprintln(w, "hi")
})
log.Fatal(http.ListenAndServe(":8080", nil))
```

Gossamer:

```gos
struct App { }

impl http::Handler for App {
    fn serve(&self, r: http::Request) -> Result<http::Response, http::Error> {
        if r.path() == "/notes" {
            return Ok(http::Response::text(200, "hi"))
        }
        Ok(http::Response::text(404, "not found"))
    }
}

fn main() -> Result<(), http::Error> {
    http::serve("0.0.0.0:8080", App { })
}
```

### sql.DB usage

Go:

```go
db, _ := sql.Open("sqlite3", ":memory:")
rows, _ := db.Query("SELECT id, body FROM notes WHERE id = ?", 1)
defer rows.Close()
for rows.Next() {
    var id int64
    var body string
    rows.Scan(&id, &body)
}
```

Gossamer:

```gos
// A SQL driver (sqlite / postgres / mysql / ...) is brought in
// as a Rust binding; the binding crate calls
// `gossamer_runtime::sql::register` at link time. After that,
// `open(name, url)` resolves to the right driver:
let mut conn = database::sql::open("sqlite", ":memory:")?
let mut rows = conn.query("SELECT id, body FROM notes WHERE id = $1",
                          &[database::sql::Value::Int(1)])?
defer rows.close()                 // like Go's defer rows.Close()
while let Some(row) = rows.next_row()? {
    let id   = row.get_i64("id")?
    let body = row.get_text("body")?
}
```

### Signal handling

Go:

```go
ch := make(chan os.Signal, 1)
signal.Notify(ch, syscall.SIGTERM)
<-ch
```

Gossamer:

```gos
// Gossamer has no user-level OS-signal handler; drive graceful
// shutdown through a cancellation channel that a coordinator
// goroutine signals.
let (shutdown_tx, shutdown_rx) = channel()
go request_shutdown(shutdown_tx)
shutdown_rx.recv()         // blocks until shutdown is signalled
```

### Structured logging

Go:

```go
logger := slog.New(slog.NewJSONHandler(os.Stdout, nil))
logger.Info("started", "port", 8080)
```

Gossamer:

```gos
// std::slog emits one JSON record per call to stdout; the trailing
// args are key/value pairs.
slog::info("started", "port", 8080)
```

## Cross-references

- [`../syntax.md`](../syntax.md) - the language tour.
- [`../codegen_abi.md`](../codegen_abi.md) - what generics fail.
- [`../stdlib_coverage.md`](../stdlib_coverage.md) - every
  stdlib item, support state.

## Standard library mapping (Go → Gossamer)

Most of Go's `net/`, `encoding/`, `compress/`, `archive/`, `crypto/`,
`hash/`, and `database/` namespace is mirrored 1:1 in Gossamer. Where
Gossamer diverges, it does so toward Rust's `fs`/`env`/`process`
split for OS primitives - `os.ReadFile` → `fs::read`, `os.Args` →
`env::args`, `os.Exit` → `process::exit`.

| Go | Gossamer |
| --- | --- |
| `os.ReadFile` | `fs::read` |
| `os.WriteFile` | `fs::write` |
| `os.Remove` | `fs::remove_file` |
| `os.MkdirAll` | `fs::create_dir_all` |
| `os.ReadDir` | `fs::read_dir` |
| `os.Args` | `env::args()` |
| `os.Getenv` | `env::var` |
| `os.Setenv` | `env::set_var` |
| `os.Getwd` | `env::current_dir` |
| `os.TempDir` | `env::temp_dir` |
| `os.Exit` | `process::exit` |
| `os/exec.Command(...)` | `process::run(prog, &args)` |
| `path/filepath.Join` | `path::join` |
| `path/filepath.Walk` | `fs::walk_dir` |
| `path/filepath.Glob` | `fs::glob` |
| `sync.Mutex` | `sync::Mutex` |
| `time.Sleep` | `time::sleep` |
| `go fn()` | `go expr` |

HTTP/2 is integrated into `std::http` directly - exactly the
shape of Go's `net/http`. `http::serve_h2c` is the cleartext h2c
entry point; HTTP/2 over TLS auto-negotiates via ALPN.
