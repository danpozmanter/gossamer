# Migrating from F# to Gossamer

F# developers will find Gossamer familiar in structure: both
languages default to immutable bindings, use `|>` for
left-to-right dataflow, and treat `Option<T>` / `Result<T, E>`
as the standard vocabulary for absent values and fallible
operations. The main shifts are concurrency model, pipe
semantics, and the absence of higher-kinded types or
computation expressions.

## TL;DR

- **What transfers directly:** `let` bindings, `|>`, pattern
  matching, discriminated unions → enums, records → structs,
  `Option<T>` / `Result<T, E>`, module-level functions.
- **What changes:** pipe threading (data-last vs. data-first),
  `go expr` replaces `async` / `Task`, traits replace interfaces,
  no curried functions, no computation expressions.
- **What's absent:** higher-kinded types, type providers,
  active patterns, units of measure, `printf`-style format
  specifiers.

## Pipe operator semantics

This is the most important difference. Both languages have `|>`,
but the argument position differs:

| Language | Desugaring |
|----------|------------|
| F# | `x \|> f` → `f x` (data is **first** arg) |
| Gossamer | `x \|> f` → `f(x)` where x fills the **last** positional slot |

F#:

```fsharp
[1; 2; 3; 4]
|> List.filter (fun n -> n % 2 = 0)
|> List.map (fun n -> n * n)
|> List.sum
```

Gossamer (data-last - each combinator's collection arg is last):

```gos
[1, 2, 3, 4]
    |> iter::filter(|n: i64| n % 2 == 0)
    |> iter::sum_by(|n: i64| n * n)
```

`List.filter f xs` in F# becomes `iter::filter(f, xs)` in
Gossamer (data last), but the `|>` threading is the same
left-to-right visual shape.

## Syntax cheat-sheet

| F# | Gossamer | Notes |
|----|----------|-------|
| `let x = 5` | `let x = 5` | Same. |
| `let mutable x = 5` | `let mut x = 5` | Same meaning. |
| `let f x y = x + y` | `fn f(x: i64, y: i64) -> i64 { x + y }` | Named params, explicit types. |
| `fun x -> x + 1` | `\|x: i64\| x + 1` | Closure syntax. |
| `if cond then a else b` | `if cond { a } else { b }` | Braces required. |
| `match x with \| A -> … \| B -> …` | `match x { A => …, B => … }` | Exhaustive. |
| `type Point = { X: int; Y: int }` | `struct Point { x: i64, y: i64 }` | |
| `type Shape = Circle of float \| Rect of float * float` | `enum Shape { Circle(f64), Rect(f64, f64) }` | |
| `printfn "%d" n` | `println!("{n}")` | |
| `sprintf "%s world" "hello"` | `format!("{} world", "hello")` or `format!("hello world")` | |
| `[1; 2; 3]` | `[1, 2, 3]` | Growable `Vec<i64>`. |
| `Map.ofList [("a", 1)]` | `HashMap::from([("a", 1)])` | |
| `async { … }` | `go fn() { … }()` | Goroutine. |
| `let! x = task` | `let (tx, rx) = channel(); let x = rx.recv()` | Channel receive. |
| `do!` / `return!` | - | Not applicable; goroutines block freely. |

## Discriminated unions → enums

F#:

```fsharp
type Tree =
    | Leaf
    | Node of int * Tree * Tree

let rec sum = function
    | Leaf -> 0
    | Node(v, l, r) -> v + sum l + sum r
```

Gossamer:

```gos
enum Tree {
    Leaf,
    Node(i64, Box<Tree>, Box<Tree>),
}

fn sum(t: &Tree) -> i64 {
    match t {
        Tree::Leaf => 0,
        Tree::Node(v, l, r) => v + sum(l) + sum(r),
    }
}
```

`Box<Tree>` spells out what F# infers for recursive variants.
The runtime owns every allocation; `Box` is a naming convention, not
a distinct heap strategy.

## Records → structs

F#:

```fsharp
type Config = {
    Host: string
    Port: int
    Verbose: bool
}

let cfg = { Host = "localhost"; Port = 8080; Verbose = false }
let updated = { cfg with Port = 9090 }
```

Gossamer:

```gos
struct Config {
    host: String,
    port: i64,
    verbose: bool,
}

let cfg = Config { host: "localhost", port: 8080, verbose: false }
let updated = Config { port: 9090, ..cfg }
```

Struct update syntax (`..base`) is supported.

## Option and Result

Both languages share `Option<T>` (Some / None) and `Result<T, E>`
(Ok / Err). Two surface differences:

- F# uses `Result.Error` as the error constructor; Gossamer uses `Err`.
- F# uses `Option.bind`, `Option.map` as free functions with the
  option as the last arg (F# currying). Gossamer uses the same
  free-function shape but threads through `|>`:

F#:

```fsharp
let parsed = input |> Option.bind tryParse |> Option.defaultValue 0
```

Gossamer:

```gos
let parsed = input
    |> option::and_then(try_parse)
    |> option::default(0)
```

`?` works for `Result` propagation exactly as in Rust.

## Async / concurrency

F# has `async { }` workflows and `Task` for .NET TPL interop.
Gossamer has goroutines - stackful coroutines scheduled by the
M:N runtime. Blocking IO doesn't block the OS thread.

F#:

```fsharp
let work = async {
    let! data = fetchAsync url
    return process data
}
Async.RunSynchronously work
```

Gossamer:

```gos
fn work(url: &String) -> Result<String, errors::Error> {
    let resp = http::get(url, &[])?
    Ok(process(&resp.body))
}

fn main() {
    let result = work(&url)
}
```

For fan-out, use goroutines + channels:

F#:

```fsharp
let results =
    urls
    |> List.map (fun url -> async { return! fetchAsync url })
    |> Async.Parallel
    |> Async.RunSynchronously
```

Gossamer:

```gos
let (tx, rx) = channel()
for url in urls {
    let tx = tx.clone()
    go fn() {
        tx.send(http::get(&url, &[]))
    }()
}

let mut results = []
for _ in urls {
    results.push(rx.recv())
}
```

## Interfaces → traits

F# uses structural typing for interfaces; any type with the right
methods satisfies the interface automatically.

Gossamer traits are nominal - you explicitly write `impl Trait
for Type { … }`:

F#:

```fsharp
type IArea =
    abstract member Area: unit -> float

type Circle(r: float) =
    interface IArea with
        member _.Area() = System.Math.PI * r * r
```

Gossamer:

```gos
trait Area {
    fn area(&self) -> f64;
}

struct Circle { r: f64 }

impl Area for Circle {
    fn area(&self) -> f64 { 3.14159 * self.r * self.r }
}
```

## Modules

F# modules are files (or `module` blocks) and open implicitly
inside the file. Gossamer modules use explicit `use` imports.
The directory layout mirrors Go: each subdirectory is a module,
and `mod.gos` is its root.

F#:

```fsharp
module Math =
    let square x = x * x
    let cube x = x * x * x
```

Gossamer (`src/math/mod.gos`):

```gos
pub fn square(x: i64) -> i64 { x * x }
pub fn cube(x: i64) -> i64 { x * x * x }
```

Consumer:

```gos
use math

fn main() {
    println!("{}", math::square(4))
}
```

## Curried functions

F# functions are curried by default: `let add x y = x + y` is
actually `int -> int -> int` and can be partially applied as
`let add5 = add 5`.

Gossamer functions are not curried. Partial application uses
closures:

```gos
fn add(x: i64, y: i64) -> i64 { x + y }
let add5 = |y: i64| add(5, y)
```

## Sequence expressions

F# `seq { … }` and `yield` build lazy sequences. Gossamer has no
lazy sequences. Use `Vec<T>` built with `iter::*` combinators or
`for` loops:

F#:

```fsharp
let evens = seq { for n in 0..100 do if n % 2 = 0 then yield n }
```

Gossamer:

```gos
let evens = iter::range_inclusive(0, 100)
    |> iter::filter(|n: i64| n % 2 == 0)
```

## What F# has that Gossamer doesn't

- **Computation expressions / monadic workflows.** Use goroutines
  + channels for sequencing async work, `iter::*` for
  collection pipelines.
- **Type providers.** No metaprogramming surface; JSON / SQL
  schemas are accessed through stdlib APIs.
- **Active patterns.** Use `fn`-based extractors + `match` guards.
- **Units of measure.** No type-level unit tracking; enforce
  units through distinct newtype wrappers.
- **Higher-kinded types.** Generic parameters are flat 64-bit
  slots in v1. See [`codegen_abi.md`](../codegen_abi.md).

## Standard library mapping (F# / .NET → Gossamer)

| F# / .NET | Gossamer |
|-----------|----------|
| `System.IO.File.ReadAllText` | `fs::read_to_string(path)` |
| `System.IO.File.WriteAllText` | `fs::write(path, data)` |
| `System.Environment.GetEnvironmentVariable` | `env::var(name)` |
| `System.Environment.GetCommandLineArgs` | `env::args()` |
| `System.Console.WriteLine` | `println!(...)` |
| `printfn "%d" n` | `println!("{n}")` |
| `sprintf "%s %d" s n` | `format!("{s} {n}")` |
| `List.map f xs` | `xs \|> iter::map(f)` |
| `List.filter f xs` | `xs \|> iter::filter(f)` |
| `List.fold f init xs` | `xs \|> iter::fold(init, f)` |
| `List.sum xs` | `xs \|> iter::sum()` |
| `Seq.length xs` | `xs.len()` |
| `Map.find k m` | `m.get(&k)` (returns `Option<&V>`) |
| `Set.contains x s` | `s.contains(&x)` |
| `String.length s` | `s.len()` |
| `String.split sep s` | `strings::split(&s, sep)` |
| `String.trim s` | `strings::trim(&s)` |
| `int.Parse s` | `strconv::parse_i64(&s)` |
| `float.Parse s` | `strconv::parse_f64(&s)` |
| `System.Threading.Tasks.Task.Run` | `go fn() { … }()` |
| `System.Threading.Thread.Sleep` | `time::sleep(ms)` |
| `System.Net.Http.HttpClient.GetAsync` | `http::get(url, headers)` |
| `System.Text.RegularExpressions.Regex` | `regex::compile(pattern)` |

## Cross-references

- [`../syntax.md`](../syntax.md) - full language tour.
- [`../stdlib_coverage.md`](../stdlib_coverage.md) - every
  stdlib module, support state.
- [`../codegen_abi.md`](../codegen_abi.md) - generic
  instantiation constraints in v1.
