# Migrating from Go to Gossamer

Gossamer takes goroutines, channels, defer, and the select
statement from Go. Most Go idioms map one-to-one. The biggest
source of friction is **syntax**: Gossamer is Rust-flavoured, so
`fn` not `func`, `let` not `:=`, `match` not `switch`, and so on.

This page walks the differences in three layers: syntax,
semantics, and stdlib equivalents. It is not exhaustive — for
the spec, see [`SPEC.md`](https://github.com/gossamer-lang/gossamer/blob/main/SPEC.md).

## TL;DR

- **What stays the same:** goroutines, channels, defer,
  select, GC, package-per-directory, structural concurrency.
- **What changes:** syntax (Rust-shaped), error handling
  (`Result<T, E>` + `?`), interfaces become traits (nominal),
  no implicit numeric coercion (`as` is explicit).
- **What's missing in v1:** HTTP/2, gRPC, database/sql, async I/O
  beyond goroutines, M:N scheduling, real package registry.

## Syntax cheat-sheet

| Go | Gossamer | Notes |
|---|---|---|
| `func name(x int) int { return x + 1 }` | `fn name(x: i64) -> i64 { x + 1 }` | Trailing-expression-as-return; `return` keyword optional. |
| `var x int = 5` / `x := 5` | `let x: i64 = 5i64` / `let x = 5i64` | `let mut` for mutables. |
| `if x > 0 { … } else { … }` | `if x > 0i64 { … } else { … }` | Same. |
| `for i := 0; i < n; i++ { … }` | `for i in 0i64..n { … }` | Range-based for-loops. |
| `for { … }` | `loop { … }` | Infinite loop. |
| `for i, v := range xs { … }` | `for (i, v) in xs.iter().enumerate() { … }` | Iterator chain. |
| `switch x { case 1: … }` | `match x { 1 => …, _ => … }` | Pattern-matching is exhaustive — a missing arm is a compile error. |
| `type Point struct { X, Y int }` | `struct Point { x: i64, y: i64 }` | Lowercase fields by convention; visibility via `pub`. |
| `func (p Point) Norm() int { … }` | `impl Point { fn norm(&self) -> i64 { … } }` | Methods declared in `impl` block. |
| `type Reader interface { Read([]byte) int }` | `trait Reader { fn read(&self, buf: &mut [u8]) -> i64 }` | Traits are nominal — `impl Reader for MyType { … }`. |
| `var err error; if err != nil { … }` | `match call() { Ok(v) => …, Err(e) => … }` | `?` propagates `Err` automatically. |
| `defer cleanup()` | `defer cleanup()` | Same syntax, same semantics. |
| `go work()` | `go work()` | Same. |
| `ch <- v` / `v := <-ch` | `tx.send(v)` / `let v = rx.recv()` | Channels are typed: `channel::<i64>()`. |
| `select { case x := <-ch: … }` | `select { recv x = ch => …, default => … }` | Select arms are typed. |
| `make([]int, 0, 16)` | `Vec::<i64>::with_capacity(16)` | Vec API. |
| `make(map[string]int)` | `HashMap::<String, i64>::new()` | HashMap API. |

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
let data = os::read_file("config.toml")?
```

`?` unwraps `Ok` or returns the `Err`. The function must declare
its return type as `Result<T, E>`.

For panicking on impossible errors, use `unwrap` or `expect`:

```gos
let data = os::read_file("config.toml").expect("config required")
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
- Generic bounds use `T: Trait` syntax: `fn handle<T: Reader>(r: T)`.
- Trait objects use `Box<dyn Trait>` or `&dyn Trait`. They have
  the same dynamic-dispatch cost as Go's interface values.

### No implicit numeric coercion

Go silently widens `int32` to `int64`, etc. Gossamer requires
`as`:

```go
var x int32 = 5
var y int64 = int64(x)   // explicit in Go too, but...
var z int64 = x          // ...this also works in Go for untyped literals
```

```gos
let x: i32 = 5i32
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
        Severity::Debug => 0i64,
        Severity::Info => 1i64,
        Severity::Warn => 2i64,
        Severity::Error => 3i64,
    }
}
```

### Concurrency

Goroutine and channel syntax is the same. Behavioural notes:

- Each goroutine is a real OS thread in v1. M:N scheduling
  arrives in v1.x. See [`perf_characteristics.md`](../perf_characteristics.md).
- Channels are unbounded by default (like Go's `make(chan T)`
  without a buffer size — wait, actually Go's *unbuffered*
  channels block on send until a receiver is ready;
  Gossamer's `channel::<T>()` returns a buffered channel today,
  with `try_send` / `try_recv` for non-blocking ops). Bounded
  channels via `channel::with_capacity(n)`.
- `select` arms are typed by the channel they reference. Catch
  the unblocked case with `default =>`.

```gos
select {
    recv v = rx => println("got:", v),
    send tx = 42i64 => println("sent"),
    default => println("would block"),
}
```

## Stdlib equivalents

| Go | Gossamer | Status |
|---|---|---|
| `fmt.Println` | `println(...)` | ✓ |
| `fmt.Printf` | `println!("{x}")` interpolation | ✓ |
| `fmt.Sprintf` | `format!("{x}")` | ✓ |
| `os.Args` | `os::args()` | ✓ |
| `os.Getenv` | `os::env(name)` | ✓ |
| `os.Exit` | `os::exit(code)` | ✓ |
| `os.ReadFile` | `os::read_file(path)` | ✓ |
| `os.WriteFile` | `os::write_file(path, data)` | ✓ |
| `os/exec.Command` | `os::exec::Command::new(prog).arg(a).output()` | v1.x |
| `os/signal.Notify` | `os::signal::on(SIGTERM)` | v1.x |
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
| `net/http` server | `http::Server::bind(addr)` | ✓ (HTTP/1.1 only) |
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
| `log/slog` | `slog::Logger::new(JsonHandler::new(io::stdout()))` | partial |
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
let data = os::read_file_to_string("input.txt")?
let n = strings::count(&data, "\n")
println(n)
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
wg.add(1i64)
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
fn handler(req: http::Request) -> http::Response {
    http::Response::ok("hello\n")
}

fn main() {
    http::serve("0.0.0.0:8080", handler)
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
    let u = User { name: "Ada".to_string(), age: 36i64 }
    let s = json::encode(&u).unwrap()
    let parsed: User = json::decode(&s).unwrap()
}
```

## Where Gossamer is harder than Go

Honest list:

- **Generics with structs `T = MyStruct` are not yet supported by
  value.** v1 monomorphisation packs every generic param into a
  64-bit slot; user structs don't fit. See
  [`codegen_abi.md`](../codegen_abi.md). Workaround: use `&T`,
  or use the runtime's `Vec<T>` / `HashMap<K, V>` which handle
  arbitrarily-sized elements internally.
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

## Cross-references

- [`../syntax.md`](../syntax.md) — the language tour.
- [`../codegen_abi.md`](../codegen_abi.md) — what generics fail.
- [`../non_goals_v1.md`](../non_goals_v1.md) — deferred features.
- [`../stdlib_coverage.md`](../stdlib_coverage.md) — every
  stdlib item, support state.
