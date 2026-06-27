# Migrating from Kotlin to Gossamer

Kotlin and Gossamer share several ideas: null safety maps to
`Option<T>`, `val`/`var` maps to `let`/`let mut`, `when`
expressions map to `match`, data classes map to structs, and
coroutines map to goroutines. The main shift is from JVM-hosted
OO with nullable types to a memory-managed compiled language where
every absent value is explicit and every blocking call is safe.

## TL;DR

- **What transfers:** `val/var` → `let/let mut`, `when` → `match`,
  data classes → structs, sealed classes → enums, lambdas → closures,
  interfaces → traits, `T?` → `Option<T>`, coroutines → goroutines.
- **What changes:** method-chaining collections vs. `iter::*` +
  `|>`, `suspend` vs. `go expr`, string templates vs. `format!`,
  companion objects vs. associated functions, exceptions vs.
  `Result<T, E>`.
- **What's absent:** `@` annotations as user-extensible metadata,
  reflection, JVM interop, `inline`/`reified` generics, Kotlin
  DSLs.

## Syntax cheat-sheet

| Kotlin | Gossamer | Notes |
|--------|----------|-------|
| `val x = 5` | `let x = 5` | Immutable binding. |
| `var x = 5` | `let mut x = 5` | Mutable binding. |
| `fun f(x: Int): Int = x + 1` | `fn f(x: i64) -> i64 { x + 1 }` | |
| `{ x: Int -> x + 1 }` | `\|x: i64\| x + 1` | Lambda / closure. |
| `if (cond) a else b` | `if cond { a } else { b }` | No parens required. |
| `when (x) { 1 -> … else -> … }` | `match x { 1 => …, _ => … }` | Exhaustive. |
| `data class Point(val x: Int, val y: Int)` | `struct Point { x: i64, y: i64 }` | |
| `sealed class Shape` | `enum Shape { … }` | |
| `println("$name is $age")` | `println!("{name} is {age}")` | |
| `"$name".length` | `name.len()` | |
| `listOf(1, 2, 3)` | `[1, 2, 3]` | `Vec<i64>`. |
| `mapOf("a" to 1)` | `HashMap::from([("a", 1)])` | |
| `x?.method()` | `if let Some(v) = x { v.method() }` | |
| `x ?: default` | `x.unwrap_or(default)` | |
| `x!!` | `x.unwrap()` | Panics on None. |
| `launch { … }` | `go fn() { … }()` | Goroutine. |
| `async { … }.await` | channel send/recv, or direct call | |
| `try { … } catch (e: …) { … }` | `match f() { Ok(v) => …, Err(e) => … }` | |
| `throw Exception("msg")` | `return Err(errors::new("msg"))` | |

## Null safety → Option<T>

Kotlin's `T?` has four operators: `?.` (safe call), `?:`
(Elvis), `!!` (force unwrap), and smart casts after null checks.
Gossamer uses `Option<T>` with explicit unwrapping:

Kotlin:

```kotlin
val len: Int? = name?.length
val display = name ?: "anonymous"
val forced = name!!.uppercase()
```

Gossamer:

```gos
let len: Option<i64> = name.map(|s: String| s.len() as i64)
let display = name.unwrap_or("anonymous")
let forced = name.unwrap().to_upper()
```

For conditional access, `if let` is idiomatic:

```gos
if let Some(n) = name {
    println!("hello, {n}")
}
```

## Data classes → structs

Kotlin:

```kotlin
data class User(val name: String, val age: Int)
val u = User("Ada", 36)
val older = u.copy(age = 37)
```

Gossamer:

```gos
struct User { name: String, age: i64 }

let u = User { name: "Ada", age: 36 }
let older = User { age: 37, ..u }
```

Deriving `Debug`, `Clone`, and `PartialEq` is the Gossamer
equivalent of data class's auto-generated `toString` /
`equals` / `hashCode`:

```gos
#[derive(Debug, Clone, PartialEq)]
struct User { name: String, age: i64 }
```

## Sealed classes → enums

Kotlin:

```kotlin
sealed class Result<out T>
data class Success<T>(val value: T) : Result<T>()
data class Failure(val error: Throwable) : Result<Nothing>()

when (result) {
    is Success -> println(result.value)
    is Failure -> println(result.error.message)
}
```

Gossamer uses the built-in `Result<T, E>` or a user enum:

```gos
enum Outcome {
    Win(String),
    Loss(String),
    Draw,
}

match outcome {
    Outcome::Win(msg) => println!("{msg}"),
    Outcome::Loss(msg) => eprintln!("{msg}"),
    Outcome::Draw => println!("tied"),
}
```

## Collections and iteration

Kotlin collections carry `.map`, `.filter`, `.fold` as methods.
Gossamer separates these into free functions in `std::iter` (data
last, piped with `|>`). Mutating helpers (`push`, `sort`,
`remove`) stay as methods.

Kotlin:

```kotlin
val total = listOf(1, 2, 3, 4, 5)
    .filter { it % 2 == 0 }
    .sumOf { it * it }
```

Gossamer:

```gos
let total = [1, 2, 3, 4, 5]
    |> iter::filter(|n: i64| n % 2 == 0)
    |> iter::sum_by(|n: i64| n * n)
```

Kotlin:

```kotlin
val words = sentence.split(" ").map { it.trim() }.filter { it.isNotEmpty() }
```

Gossamer:

```gos
let words = strings::split(&sentence, " ")
    |> iter::map(|s: String| strings::trim(&s))
    |> iter::filter(|s: String| s.len() > 0)
```

## String interpolation

Kotlin uses `"$variable"` and `"${expression}"`. Gossamer uses
`format!` / `println!` with `{identifier}` or positional `{}`:

Kotlin:

```kotlin
val msg = "Hello, $name! You are ${age + 1} next year."
```

Gossamer:

```gos
let msg = format!("Hello, {name}! You are {} next year.", age + 1)
```

## Coroutines → goroutines

Kotlin coroutines are cooperative and tied to a `CoroutineScope`.
Gossamer goroutines are stackful and scheduled by an M:N
work-stealing runtime - blocking IO doesn't block the OS thread.

Kotlin:

```kotlin
fun main() = runBlocking {
    val result = async { fetchData(url) }
    println(result.await())
}
```

Gossamer:

```gos
fn main() {
    let (tx, rx) = channel()
    go fn() {
        tx.send(fetch_data(&url))
    }()
    println!("{}", rx.recv().unwrap())
}
```

For fan-out + fan-in, WaitGroup is the Go-shaped idiom:

```gos
let wg = sync::WaitGroup::new()
let (tx, rx) = channel()

for url in urls {
    wg.add(1)
    let tx = tx.clone()
    go fn() {
        defer wg.done()
        tx.send(fetch_data(&url))
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

## Extension functions

Kotlin extension functions add methods to existing types:

```kotlin
fun String.shout(): String = this.uppercase() + "!"
```

Gossamer uses `impl` blocks or standalone free functions:

```gos
fn shout(s: &String) -> String {
    strings::to_upper(s) + "!"
}
```

If the method belongs logically to a type you own, use `impl`:

```gos
impl MyType {
    pub fn shout(&self) -> String {
        strings::to_upper(&self.name) + "!"
    }
}
```

## Companion objects → associated functions

Kotlin companion objects hold factory methods and constants:

```kotlin
class Connection private constructor(val host: String) {
    companion object {
        fun local() = Connection("localhost")
        const val DEFAULT_PORT = 5432
    }
}
```

Gossamer uses associated functions on `impl`:

```gos
struct Connection { host: String }

impl Connection {
    pub fn local() -> Connection {
        Connection { host: "localhost" }
    }
}

const DEFAULT_PORT: i64 = 5432
```

## Exceptions → Result

Kotlin uses exceptions for errors. Gossamer uses `Result<T, E>`
with `?` for propagation:

Kotlin:

```kotlin
fun readConfig(path: String): Config {
    val text = File(path).readText()  // throws IOException
    return parseConfig(text)         // throws ParseException
}
```

Gossamer:

```gos
fn read_config(path: &String) -> Result<Config, errors::Error> {
    let text = fs::read_to_string(path)?
    parse_config(&text)
}
```

`?` unwraps `Ok(v)` or returns `Err(e)` from the enclosing
function. Combine errors with `errors::wrap`:

```gos
fn read_config(path: &String) -> Result<Config, errors::Error> {
    let text = fs::read_to_string(path)
        .map_err(|e| errors::wrap(e, format!("reading {path}")))?
    parse_config(&text)
}
```

## Standard library mapping (Kotlin / JVM → Gossamer)

| Kotlin / JVM | Gossamer |
|--------------|----------|
| `File(path).readText()` | `fs::read_to_string(path)` |
| `File(path).writeText(s)` | `fs::write(path, s)` |
| `File(path).delete()` | `fs::remove_file(path)` |
| `File(path).mkdirs()` | `fs::create_dir_all(path)` |
| `System.getenv("X")` | `env::var("X")` |
| `System.exit(0)` | `process::exit(0)` |
| `ProcessBuilder(cmd).start()` | `process::Command::new(cmd).spawn()` |
| `println(x)` | `println!("{x}")` |
| `System.currentTimeMillis()` | `time::now().as_millis()` |
| `Thread.sleep(ms)` | `time::sleep(ms)` |
| `Regex(pattern).matches(s)` | `regex::compile(pattern)?.is_match(&s)` |
| `s.split(delim)` | `strings::split(&s, delim)` |
| `s.trim()` | `strings::trim(&s)` |
| `s.uppercase()` | `strings::to_upper(&s)` |
| `s.toInt()` | `strconv::parse_i64(&s)` |
| `n.toString()` | `strconv::format_i64(n)` |
| `listOf(...)` | `[...]` (`Vec<T>`) |
| `mutableListOf(...)` | `let mut xs = [...]` |
| `mapOf(k to v)` | `HashMap::from([(k, v)])` |
| `setOf(...)` | `HashSet::from([...])` |
| `list.map { }` | `list \|> iter::map(\|x\| …)` |
| `list.filter { }` | `list \|> iter::filter(\|x\| …)` |
| `list.fold(init) { acc, x -> }` | `list \|> iter::fold(init, \|acc, x\| …)` |
| `list.sortedBy { }` | `xs.sort_by(\|a, b\| …)` |
| `OkHttp / Ktor HttpClient` | `http::Client::new()` |
| `ktor server { }` | `http::serve(addr, handler)` |
| `kotlinx.serialization` | `encoding::json::encode(v)` |
| `Gson / Jackson` | `encoding::json::decode::<T>(s)` |
| `kotlinx.coroutines.launch` | `go fn() { … }()` |
| `Mutex()` | `sync::Mutex::new()` |
| `CountDownLatch(n)` | `sync::WaitGroup::new()` |

## Cross-references

- [`../syntax.md`](../syntax.md) - full language tour.
- [`../stdlib_coverage.md`](../stdlib_coverage.md) - every
  stdlib module, support state.
- [`../codegen_abi.md`](../codegen_abi.md) - generic
  instantiation constraints in v1.
