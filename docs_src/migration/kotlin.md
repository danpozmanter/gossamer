# Migrating from Kotlin to Gossamer

Kotlin and Gossamer share null-safe habits, expression-oriented
control flow, lambdas, and pattern-style branching. Gossamer is not
JVM-hosted and does not use exceptions or `suspend`; it uses explicit
`Option<T>`, `Result<T, E>`, goroutines, and channels.

## Quick Map

| Kotlin | Gossamer |
| --- | --- |
| `val x = 5` | `let x = 5` |
| `var x = 5` | `let mut x = 5` |
| `fun f(x: Int): Int = x + 1` | `fn f(x: i64) -> i64 { x + 1 }` |
| `{ x: Int -> x + 1 }` | `|x: i64| x + 1` |
| `if (c) a else b` | `if c { a } else { b }` |
| `when (x) { ... }` | `match x { ... }` |
| `data class User(...)` | `struct User { ... }` |
| `User("Ada", 36)` | `User { name: "Ada", age: 36 }` |
| `sealed class` | `enum` |
| `T?` | `Option<T>` |
| `try` / `catch` | `Result<T, E>` |
| `launch { ... }` | `go fn() { ... }()` |
| `async { ... }.await()` | channel send and receive |
| `println("$name")` | `println!("{name}")` |

## Gossamer 0.37 Syntax At A Glance

Kotlin uses semicolons optionally and commas in parameter lists, data classes,
and collection literals. Gossamer rejects semicolons. Commas are required in a
delimited list on one line; newlines separate items in a multiline list.
Multiline commas are accepted for migration, but `gos fmt` removes them.

```kotlin
data class User(
    val name: String,
    val active: Boolean,
)
val user = User(name = "Ada", active = true)
```

```gos
struct User {
    name: String
    active: bool
}

fn rename(
    user: User
    name: String
) -> User {
    User {
        name: name
        active: user.active
    }
}

enum Lookup {
    Found {
        index: i64
        user: User
    }
    Missing(String)
}

let user = User { name: "Ada", active: true } // one line needs commas
```

Named structs use keyed braces. Tuple structs and tuple enum variants use
parentheses. Collection and product access stays explicit:

```kotlin
val first = users[0]
val enabled = pair.second
val cached = byName["Ada"] // User?
```

```gos
let users = [user, rename(user, "Grace")]
let first = users[0]              // List/Vec index; traps if out of bounds
let initial = first.name[0]       // String index is a UTF-8 byte as i64
let pair = (first.name, first.active)
let enabled = pair.1
let mut by_name: HashMap<String, User> = HashMap::new()
by_name.insert(first.name, first)
let cached = by_name.get("Ada")   // HashMap lookup returns Option<V>
let found = Lookup::Found {
    index: 0
    user: cached.unwrap()
}
```

## Null Safety To Option

Kotlin:

```kotlin
val len = name?.length
val display = name ?: "anonymous"
val forced = name!!.uppercase()
```

Gossamer:

```gos
let len = name.map(|s: String| s.len())
let display = name.unwrap_or("anonymous")
let forced = strings::to_uppercase(&name.unwrap())

if let Some(n) = name {
    println!("hello, {n}")
}
```

`None` only exists inside `Option<T>`. It is not a universal null.

## Data Classes To Structs

```kotlin
data class User(val name: String, val age: Int)
val u = User("Ada", 36)
val older = u.copy(age = 37)
```

```gos
struct User {
    name: String
    age: i64
}

let u = User { name: "Ada", age: 36 }
let older = User { age: 37, ..u }
```

Named structs use braces. Tuple-like parentheses are for enum tuple
variants and tuple structs, not ordinary named structs.

## Sealed Classes To Enums

```kotlin
sealed class Outcome
data class Win(val message: String) : Outcome()
data class Loss(val message: String) : Outcome()
object Draw : Outcome()
```

```gos
enum Outcome {
    Win(String)
    Loss(String)
    Draw
}

match outcome {
    Outcome::Win(message) => println!("{message}")
    Outcome::Loss(message) => eprintln!("{message}")
    Outcome::Draw => println!("draw")
}
```

`match` is exhaustive, so adding a new enum variant forces call sites to
handle it.

## Exceptions To Result

Kotlin:

```kotlin
fun readConfig(path: String): Config {
    val text = File(path).readText()
    return parseConfig(text)
}
```

Gossamer:

```gos
use std::{errors, fs}

fn read_config(path: &String) -> Result<Config, errors::Error> {
    let text = fs::read_to_string(path)?
    parse_config(&text)
}
```

Use `match` when a caller should recover locally:

```gos
let cfg = match read_config(&path) {
    Ok(v) => v
    Err(e) => {
        eprintln!("config error: {e}")
        default_config()
    }
}
```

## Collections

Kotlin collection chains become `std::iter` pipelines. The pipe operator
passes the value on the left into the last argument of the next call.

```kotlin
val total = listOf(1, 2, 3, 4)
    .filter { it % 2 == 0 }
    .sumOf { it * it }
```

```gos
use std::iter

let total = [1, 2, 3, 4]
    |> iter::filter(|n: i64| n % 2 == 0)
    |> iter::sum_by(|n: i64| n * n)
```

Mutating operations stay method-shaped:

```gos
let mut xs = [3, 1, 2]
xs.sort()
xs.push(4)
```

## Coroutines To Goroutines

```kotlin
fun main() = runBlocking {
    val result = async { fetchData(url) }
    println(result.await())
}
```

```gos
let (tx, rx) = channel()

go fn() {
    tx.send(fetch_data(&url))
    tx.close()
}()

if let Some(result) = rx.recv() {
    println!("{result}")
}
```

For fan-out and fan-in, use `sync::WaitGroup`:

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
    handle(result)
}
```

## Associated Functions

Kotlin companion factories become associated functions:

```gos
struct Connection { host: String }

impl Connection {
    pub fn local() -> Connection {
        Connection { host: "localhost" }
    }
}

const DEFAULT_PORT: i64 = 5432
```

## Standard Library Map

| Kotlin / JVM | Gossamer |
| --- | --- |
| `File(path).readText()` | `fs::read_to_string(path)` |
| `File(path).readBytes()` | `fs::read(path)` |
| `File(path).writeText(s)` | `fs::write(path, s)` |
| `System.getenv("X")` | `env::var("X")` |
| `ProcessBuilder(cmd).start()` | `process::run(cmd, &args)` |
| `System.exit(0)` | `process::exit(0)` |
| `println(x)` | `println!("{x}")` |
| `Regex(pattern)` | `regex::compile(pattern)` |
| `s.trim()` | `strings::trim(&s)` |
| `s.uppercase()` | `strings::to_uppercase(&s)` |
| `s.toInt()` | `strconv::parse_i64(&s)` |
| `listOf(...)` | `[...]` |
| `mutableListOf(...)` | `let mut xs = [...]` |
| `mapOf(k to v)` | `HashMap::new()` plus `insert` |
| `setOf(...)` | `HashSet::new()` plus `insert` |
| `OkHttp` / `Ktor HttpClient` | `http::Client::new()` or `http::get(url, [])` |
| `ktor server { ... }` | `http::serve(addr, handler)` |
| `kotlinx.serialization` | `encoding::json` |
| `kotlinx.coroutines.launch` | `go fn() { ... }()` |
| `Mutex()` | `sync::Mutex::new()` |
| `CountDownLatch(n)` | `sync::WaitGroup::new()` |
