# Migrating from F# to Gossamer

F# and Gossamer both favor immutable `let` bindings, pattern matching,
`Option<T>`, `Result<T, E>`, and left-to-right pipelines. The main
differences are syntax, pipe argument order, concurrency, and the
absence of F# metaprogramming features.

## Quick Map

| F# | Gossamer | Notes |
| --- | --- | --- |
| `let x = 5` | `let x = 5` | Same. |
| `let mutable x = 5` | `let mut x = 5` | Same meaning. |
| `let f x y = x + y` | `fn f(x: i64, y: i64) -> i64 { x + y }` | Functions are not curried. |
| `fun x -> x + 1` | `|x: i64| x + 1` | Closure syntax. |
| `if c then a else b` | `if c { a } else { b }` | Braces required. |
| `match x with | A -> ...` | `match x { A => ... }` | Exhaustive. |
| record | `struct` | Construct with braces. |
| discriminated union | `enum` | Tuple variants use parentheses. |
| `async { ... }` | `go fn() { ... }()` | Goroutine. |
| `Task` result | channel receive or direct `Result` | Blocking calls are acceptable. |
| `printfn "%d" n` | `println!("{n}")` | Format strings are Rust-like. |

## Pipe Operator

F# pipes into the next function's first argument. Gossamer pipes into
the next function's last positional argument.

```fsharp
[1; 2; 3; 4]
|> List.filter (fun n -> n % 2 = 0)
|> List.sumBy (fun n -> n * n)
```

```gos
use std::iter

[1, 2, 3, 4]
    |> iter::filter(|n: i64| n % 2 == 0)
    |> iter::sum_by(|n: i64| n * n)
```

This is why Gossamer stdlib pipeline helpers put the data argument
last.

## Records To Structs

```fsharp
type Config = {
    Host: string
    Port: int
    Verbose: bool
}

let cfg = { Host = "localhost"; Port = 8080; Verbose = false }
let updated = { cfg with Port = 9090 }
```

```gos
struct Config {
    host: String
    port: i64
    verbose: bool
}

let cfg = Config { host: "localhost", port: 8080, verbose: false }
let updated = Config { port: 9090, ..cfg }
```

Named structs use braces with keyed fields:

```gos
struct Pair { left: i64, right: i64 }

let a = Pair { left: 1, right: 2 }
```

Parentheses are reserved for tuple structs and enum tuple variants.

## Discriminated Unions To Enums

```fsharp
type Tree =
    | Leaf
    | Node of int * Tree * Tree
```

```gos
enum Tree {
    Leaf,
    Node(i64, Tree, Tree),
}

fn sum(t: &Tree) -> i64 {
    match t {
        Tree::Leaf => 0,
        Tree::Node(v, l, r) => v + sum(l) + sum(r),
    }
}
```

Recursive enum variants are runtime-managed. Add `Box<T>` only when it
makes a public type clearer.

## Option And Result

Both languages share the same vocabulary:

```fsharp
let parsed = input |> Option.bind tryParse |> Option.defaultValue 0
```

```gos
use std::option

let parsed = input
    |> option::and_then(try_parse)
    |> option::unwrap_or(0)
```

For fallible work, `?` is usually clearer than a pipeline:

```gos
fn load(path: &String) -> Result<Config, errors::Error> {
    let text = fs::read_to_string(path)?
    parse_config(&text)
}
```

## Concurrency

Gossamer has stackful goroutines and channels instead of computation
expressions:

```gos
let wg = sync::WaitGroup::new()
let (tx, rx) = channel()

for url in urls {
    wg.add(1)
    let tx = tx.clone()
    go fn() {
        defer wg.done()
        tx.send(http::get(&url, []))
    }()
}

go fn() {
    wg.wait()
    tx.close()
}()

while let Some(result) = rx.recv() {
    process(result)
}
```

## Traits

F# interfaces are object-oriented. Gossamer traits are nominal and
implemented explicitly:

```gos
trait Area {
    fn area(&self) -> f64
}

struct Circle { r: f64 }

impl Area for Circle {
    fn area(&self) -> f64 { 3.14159 * self.r * self.r }
}
```

Generic bounds use `T: Area`. For a closed set of cases, prefer an
`enum` and exhaustive `match`.

## Missing F# Features

Gossamer does not have computation expressions, active patterns, units
of measure, type providers, higher-kinded types, or curried functions.
Use closures for partial application:

```gos
fn add(x: i64, y: i64) -> i64 { x + y }
let add5 = |y: i64| add(5, y)
```

## Standard Library Map

| F# / .NET | Gossamer |
| --- | --- |
| `System.IO.File.ReadAllText` | `fs::read_to_string(path)` |
| `System.IO.File.WriteAllText` | `fs::write(path, data)` |
| `Environment.GetEnvironmentVariable` | `env::var(name)` |
| `Environment.GetCommandLineArgs` | `env::args()` |
| `Console.WriteLine` | `println!(...)` |
| `sprintf "%s %d" s n` | `format!("{s} {n}")` |
| `List.map f xs` | `xs |> iter::map(f)` |
| `List.filter f xs` | `xs |> iter::filter(f)` |
| `List.fold f init xs` | `xs |> iter::fold(init, f)` |
| `Map.find k m` | `m.get(&k)` |
| `Set.contains x s` | `s.contains(&x)` |
| `String.trim s` | `strings::trim(&s)` |
| `int.Parse s` | `strconv::parse_i64(&s)` |
| `Task.Run` | `go fn() { ... }()` |
| `Thread.Sleep(ms)` | `time::sleep(ms)` |
| `HttpClient.GetAsync(url)` | `http::get(url, [])` |
| `Regex(pattern)` | `regex::compile(pattern)` |
