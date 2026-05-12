# Migrating from Python to Gossamer

Python programs map well to Gossamer once you commit to three shifts:
static types, no implicit `None`, and pattern matching instead of
`isinstance` / duck-typing.

## Differences that matter

| Python | Gossamer |
|--------|----------|
| Dynamic typing, runtime `TypeError`. | Static typing checked by `gos check`; most type errors are caught before run. |
| `None` is implicit and pervasive. | `Option<T>` is explicit. `None` exists only for `Option`. |
| `try / except`. | `Result<T, E>` + `?`. No exception-style control flow. |
| Duck typing via `__dunder__` methods. | Traits (`impl Iterator for T`, `impl Display for T`). |
| `str` is a sequence of code points; indexing is cheap. | `String` is UTF-8; indexing returns the byte at that offset. Use `std::utf8` helpers for code-point iteration. |
| `dict`, `list`, `set` are built-ins. | `std::collections::HashMap`, `Vec<T>` (spelled `[T]` in types), `HashSet<T>`. |
| Decorators, metaclasses. | Attributes (`#[test]`, `#[cfg(...)]`, `#[lint(allow(...))]`). Not user-extensible. |
| Indentation is syntax. | Braces `{ }` are syntax; `gos fmt` enforces a consistent shape. |
| `async def` / `await`. | `go expr` spawns a goroutine; blocking IO is fine. |

## What stays the same

- List / dict / set comprehensions have direct iterator equivalents (`xs.filter(...).map(...).collect()`).
- `print("a", "b")` → `println("a", "b")`.
- Named arguments → struct-literal call style: `Handler { log: true }`.

## Translation examples

Python:

```python
def parse(line):
    parts = line.strip().split(",")
    return [int(p) for p in parts]
```

Gossamer:

```gos
fn parse(line: &str) -> Result<[i64], errors::Error> {
    let mut out: [i64] = []
    for piece in line.trim().split(',') {
        let n: i64 = piece.trim().parse()
            .map_err(|_| errors::new(format!("bad number: {piece}")))?
        out.push(n)
    }
    Ok(out)
}
```

Python:

```python
try:
    value = risky()
except ValueError as e:
    log(e)
    value = default
```

Gossamer:

```gos
let value = match risky() {
    Ok(v) => v,
    Err(e) => { log(&e); default },
}
```

## List comprehensions become `|>` chains

Python:

```python
total = sum(n * n for n in range(1, 11) if n % 2 == 0)
```

Gossamer:

```gos
let total = iter::range_inclusive(1, 10)
    |> iter::filter(|n: i64| n % 2 == 0)
    |> iter::sum_by(|n: i64| n * n)
```

Python:

```python
names = sorted({name.lower() for name in users if name})
```

Gossamer (rough mirror — `|>` threads each stage):

```gos
let names = users
    |> iter::filter(|n: String| n.len() > 0)
    |> iter::map(|n: String| n.to_lowercase())
    |> iter::sort_by_key(|n: String| n.len())
```

The `iter::*` module is small and uniform: combinators take the
data value as the last positional parameter so `xs |> iter::map(f)`
desugars to `iter::map(f, xs)` (SPEC §4.6). For the optional /
fallible cases there are matching `std::option` and `std::result`
modules.
