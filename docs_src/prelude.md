# Builtins and prelude

Gossamer 0.28.2 puts these names in every file. No `use` needed. A local
definition with the same name wins.

## Output and formatting

Format strings use `{}` and `{name}` placeholders.

| Name | Signature | Description |
|---|---|---|
| `println!` | `println!("fmt", values...)` | Print formatted text to stdout, then a newline. |
| `print!` | `print!("fmt", values...)` | Print formatted text to stdout with no newline. |
| `eprintln!` | `eprintln!("fmt", values...)` | Print formatted text to stderr, then a newline. |
| `eprint!` | `eprint!("fmt", values...)` | Print formatted text to stderr with no newline. |
| `format!` | `format!("fmt", values...) -> String` | Render formatted text into an owned `String`. |
| `panic!` | `panic!("fmt", values...) -> !` | Stop the current goroutine with the rendered message. |

```gossamer
let who = "world"
println!("hello, {who}")
println!("{} + {} = {}", 1, 2, 1 + 2)
```

## Fixed macros

| Name | Signature | Description |
|---|---|---|
| `matches!` | `matches!(expr, pattern) -> bool` | Test whether `expr` matches `pattern`. |
| `todo!` | `todo!("msg"?) -> !` | Mark code as intentionally unfinished and panic if reached. |
| `unimplemented!` | `unimplemented!("msg"?) -> !` | Mark an unsupported path and panic if reached. |
| `unreachable!` | `unreachable!("msg"?) -> !` | Mark an impossible path and panic if reached. |
| `dbg!` | `dbg!(expr) -> T` | Print `expr` with debug formatting, then return it. |
| `regex!` | `regex!("pattern") -> regex::Pattern` | Compile a checked regular expression at build time. |
| `sql!` | `sql!("query")` | Check a SQL literal at build time when a driver can validate it. |
| `codegen!` | `codegen!(...)` | Run the build-time codegen hook. |

User-defined macros do not exist. Any other `name!(...)` is a parse error.

## Assertions

| Name | Signature | Description |
|---|---|---|
| `assert` | `assert(cond: bool, msg?: String)` | Panic when `cond` is false. |
| `assert_eq` | `assert_eq(a, b, msg?: String)` | Panic when `a != b`; include both values in the failure text. |

Use `todo!` for unfinished code. There is no `todo()` function.

## Scalar helpers

| Name | Signature | Description |
|---|---|---|
| `min` | `min(a, b) -> T` | Return the smaller comparable scalar. |
| `max` | `max(a, b) -> T` | Return the larger comparable scalar. |
| `min` | `min(xs) -> Option<T>` | Return the smallest collection item, or `None` when empty. |
| `max` | `max(xs) -> Option<T>` | Return the largest collection item, or `None` when empty. |
| `clamp` | `clamp(x, lo, hi) -> T` | Limit `x` to the inclusive range `[lo, hi]`. |

```gossamer
let speed = clamp(input, 0, 120)
let better = max(score_a, score_b)
```

## Concurrency

| Name | Signature | Description |
|---|---|---|
| `go` | `go expr` | Run `expr` on a goroutine and discard its result. |
| `spawn` | `spawn(f) -> JoinHandle<T>` | Run `f` on a goroutine and return a join handle. |
| `join` | `handle.join() -> Result<T, String>` | Wait for a spawned goroutine. `Err` carries the panic message. |

```gossamer
let h = spawn(|| heavy_compute())
match h.join() {
    Ok(v) => println!("{v}"),
    Err(e) => eprintln!("worker panicked: {e}"),
}
```

`Sender`, `Receiver`, `Mutex`, and `WaitGroup` type names resolve in
signatures without imports. Their constructors and helpers live in `std::sync`.

## Serialization

Every user `struct` gets strict typed codecs. No derive attribute needed.

| Name | Signature | Description |
|---|---|---|
| `from_json` | `from_json::<T>(&text) -> Result<T, errors::Error>` | Decode JSON into `T`; report missing fields and type mismatches. |
| `to_json` | `to_json::<T>(&value) -> Result<String, errors::Error>` | Encode `T` as JSON text. |
| `from_toml` | `from_toml::<T>(&text) -> Result<T, errors::Error>` | Decode TOML into `T`; report schema errors. |
| `to_toml` | `to_toml::<T>(&value) -> Result<String, errors::Error>` | Encode `T` as TOML text. |
| `from_yaml` | `from_yaml::<T>(&text) -> Result<T, errors::Error>` | Decode YAML into `T`; report schema errors. |
| `to_yaml` | `to_yaml::<T>(&value) -> Result<String, errors::Error>` | Encode `T` as YAML text. |

```gossamer
struct Config { host: String, port: i64 }

let cfg = from_json::<Config>(&text)?
```

Use `std::encoding::json` for dynamic JSON where the shape is not known at
compile time.

## Always-in-scope types

| Family | Names | Description |
|---|---|---|
| Primitives | `bool`, `char`, signed and unsigned integers, `isize`, `usize`, `f32`, `f64`, `String`, `str` | Scalar and text types. |
| Wrappers | `Option<T>`, `Result<T, E>`, `Box<T>`, `Rc<T>`, `Arc<T>`, `Weak<T>` | Sum types and managed-runtime compatibility wrappers. |
| Collections | `Vec<T>`, `HashMap<K, V>`, `HashSet<T>`, `BTreeMap<K, V>`, `VecDeque<T>`, `Range` | Core collection types. |
| Concurrency | `Sender<T>`, `Receiver<T>`, `Mutex<T>`, `WaitGroup`, `JoinHandle<T>` | Channel, lock, wait, and goroutine-handle types. |

## Runtime statements

| Statement | Description |
|---|---|
| `defer expr` | Run `expr` on every exit path of the enclosing block. |
| `arena { ... }` | Allocate values inside a region and free them when the block exits. |
| `select { ... }` | Wait on multiple channel operations. |
