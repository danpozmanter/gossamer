# Migrating from Python to Gossamer

Python code ports cleanly once three habits change: types are checked
before execution, absence is represented with `Option<T>`, and failures
are returned as `Result<T, E>` instead of raised as exceptions.

## Quick Map

| Python | Gossamer |
| --- | --- |
| `x = 5` | `let x = 5` |
| reassigning `x` | `let mut x = 5` |
| `def f(x): return x + 1` | `fn f(x: i64) -> i64 { x + 1 }` |
| `class User: ...` for data | `struct User { name: String, age: i64 }` |
| `User("Ada", 36)` | `User { name: "Ada", age: 36 }` |
| `None` | `Option<T>` with `Some(v)` or `None` |
| `try` / `except` | `Result<T, E>` with `?` or `match` |
| `isinstance` dispatch | `enum` plus `match`, or traits |
| list | `[T]` / `Vec<T>` |
| dict | `HashMap<K, V>` |
| set | `HashSet<T>` |
| `asyncio.create_task` | `go fn() { ... }()` |
| `if __name__ == "__main__"` | entry-file top-level statements |

## Data Types

Use structs for records. Named structs are always constructed with
braces:

```gos
struct User {
    name: String,
    age: i64,
}

let user = User { name: "Ada", age: 36 }
let older = User { age: 37, ..user }
```

Use enums for a closed set of shapes:

```gos
enum Event {
    Click(i64, i64),
    Message(String),
    Closed,
}

match event {
    Event::Click(x, y) => println!("click {x} {y}"),
    Event::Message(text) => println!("{text}"),
    Event::Closed => println!("closed"),
}
```

## Option Instead Of None

```python
name = user.get("name")
display = name or "anonymous"
```

```gos
let name: Option<String> = user_name()
let display = name.unwrap_or("anonymous")

if let Some(n) = name {
    println!("hello, {n}")
}
```

`None` is not a value of every type. It only appears inside
`Option<T>`.

## Result Instead Of Exceptions

```python
try:
    text = Path(path).read_text()
    cfg = parse_config(text)
except Exception as e:
    log(e)
    cfg = default_config()
```

```gos
use std::{errors, fs}

fn read_config(path: &String) -> Result<Config, errors::Error> {
    let text = fs::read_to_string(path)?
    parse_config(&text)
}

let cfg = match read_config(&path) {
    Ok(v) => v,
    Err(e) => {
        log(&e)
        default_config()
    },
}
```

## Comprehensions And Pipelines

Python:

```python
total = sum(n * n for n in range(1, 11) if n % 2 == 0)
```

Gossamer:

```gos
use std::iter

let total = iter::range_inclusive(1, 10)
    |> iter::filter(|n: i64| n % 2 == 0)
    |> iter::sum_by(|n: i64| n * n)
```

For stateful code, ordinary loops are still idiomatic:

```gos
let mut counts: HashMap<String, i64> = HashMap::new()
for word in words {
    counts.inc(word, 1)
}
```

## Strings And Bytes

`String` is UTF-8. Indexing works on bytes, not Python code points.
Use UTF-8 helpers when code-point semantics matter. Use `[u8]` for
binary data.

```gos
let body: [u8] = fs::read("image.bin")?
let text = fs::read_to_string("message.txt")?
```

HTTP responses can serve binary bodies directly:

```gos
http::Response {
    status: 200,
    body: [65, 0, 66],
    content_type: "application/octet-stream",
}
```

## Concurrency

Python `async` code usually becomes goroutines plus channels when work
must run concurrently:

```gos
let (tx, rx) = channel()

for url in urls {
    let tx = tx.clone()
    go fn() {
        tx.send(http::get(&url, []))
    }()
}

while let Some(result) = rx.recv() {
    handle(result)
}
```

Close the sender when no more values will arrive, or coordinate with
`sync::WaitGroup`.

## Standard Library Map

| Python | Gossamer |
| --- | --- |
| `Path(path).read_text()` | `fs::read_to_string(path)` |
| `Path(path).read_bytes()` | `fs::read(path)` |
| `Path(path).write_text(s)` | `fs::write(path, s)` |
| `os.environ.get("X")` | `env::var("X")` |
| `sys.argv` | `env::args()` |
| `subprocess.run([...])` | `process::run(program, &args)` |
| `print(x)` | `println!("{x}")` |
| `json.dumps(v)` | `encoding::json::encode(v)` |
| `json.loads(s)` | `encoding::json::decode::<T>(s)` |
| `re.compile(p)` | `regex::compile(p)` |
| `s.strip()` | `strings::trim(&s)` |
| `int(s)` | `strconv::parse_i64(&s)` |
