# Prelude — available without imports

Every Gossamer program starts with these names in scope. No `use`
needed.

## Output and failure

The six macros (the only macros in the language), all
`format!`-style with `{}` / `{name}` placeholders:

| Macro | Effect |
|-------|--------|
| `println!("…", a)` | stdout, newline appended |
| `print!("…", a)` | stdout, no newline |
| `eprintln!("…", a)` | stderr, newline appended |
| `eprint!("…", a)` | stderr, no newline |
| `format!("…", a)` | returns the rendered `String` |
| `panic!("…", a)` | unwinds with the rendered message |

```gossamer
let who = "world"
println!("hello, {who}")
println!("{} + {} = {}", 1, 2, 1 + 2)
```

## Assertions

- `assert(cond)` — panics when `cond` is false.
- `assert_eq(a, b)` — panics when `a != b`, printing both values.
- `todo()` — panics with a "not yet implemented" marker.

## Scalars

- `min(a, b)` / `max(a, b)` — work on any pair of comparable
  scalars. The one-argument collection forms `min(xs)` / `max(xs)`
  return `Option<T>`.
- `clamp(x, lo, hi)` — `x` limited to `[lo, hi]`.

```gossamer
let speed = clamp(input, 0, 120)
let better = max(score_a, score_b)
```

## Concurrency

- `go expr` — statement keyword: run `expr` on a goroutine,
  fire-and-forget.
- `spawn(f)` — run `f` on a goroutine and get a `JoinHandle<T>`;
  `handle.join()` blocks for `Result<T, String>` (`Err` carries the
  panic message if the goroutine panicked).

```gossamer
let h = spawn(|| heavy_compute())
match h.join() {
    Ok(v) => println!("{v}"),
    Err(e) => eprintln!("worker panicked: {e}"),
}
```

Channels, `Mutex`, and `WaitGroup` come from `std::sync`, but the
`Sender` / `Receiver` / `Mutex` / `WaitGroup` *type names* resolve
without an import when they appear in signatures.

## Serialization (synthesized per struct)

Every user `struct` automatically gets free functions callable with
a turbofish — no import, no derive attribute:

| Function | Returns |
|----------|---------|
| `from_json::<T>(&text)` | `Result<T, errors::Error>` |
| `to_json::<T>(&value)` | `Result<String, errors::Error>` |
| `from_toml::<T>` / `to_toml::<T>` | same shapes, TOML |
| `from_yaml::<T>` / `to_yaml::<T>` | same shapes, YAML |

```gossamer
struct Config { host: String, port: i64 }

let cfg = from_json::<Config>(&text)?
```

Decoding is strict: missing required fields and type mismatches
return `Err` naming the offending field. The dynamic surface
(`json::parse`, `json::get`, `json::as_i64`, …) lives in
`std::encoding::json` for documents whose shape is not known at
compile time.

## Types

Always-in-scope type names:

- Primitives: `bool`, `char`, `i8`…`i128`, `u8`…`u128`, `isize`,
  `usize`, `f32`, `f64`, `String`, `str`.
- Wrappers: `Option<T>` (`Some` / `None`), `Result<T, E>`
  (`Ok` / `Err`), `Box<T>`, `Rc<T>`, `Arc<T>` (all three are
  transparent in a GC'd runtime — spelling compatibility with
  Rust), `Weak<T>`.
- Collections: `Vec<T>`, `HashMap<K, V>`, `HashSet<T>`,
  `BTreeMap<K, V>`, `BTreeSet<T>`, `VecDeque<T>`, `Range`.
- Concurrency: `Sender<T>`, `Receiver<T>`, `Mutex<T>`,
  `WaitGroup`, `JoinHandle<T>`.

## Statement keywords with runtime effects

Not functions, but also available everywhere:

- `defer expr` — run `expr` on every exit path of the enclosing
  block.
- `arena { … }` — bump-allocate everything created inside; free it
  wholesale at the block's exit ([memory model](memory.md)).
- `select { … }` — multiplex channel operations.

A user definition with the same name shadows any prelude entry —
prelude bindings never collide with your code.
