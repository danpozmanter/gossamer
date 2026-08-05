# `lang::tuple`

Fixed-length group of values whose element types may differ.
<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

A tuple is written with parentheses and needs no import. Its type is the
parenthesised list of its element types, so `(1, "two", 3.0)` has type
`(i64, String, f64)`.

```gossamer
let entry = (1, "two", 3.0)
println!("{} {} {}", entry.0, entry.1, entry.2)
```

Unlike a `Vec<T>`, a tuple's length is part of its type and its elements do not
have to share a type. Unlike a struct, its fields are positional and it needs no
declaration.

## Construction

```gossamer
let pair = (3, 4)          // (i64, i64)
let mixed = (1, "two", 3.0) // (i64, String, f64)
let single = (5,)           // one-element tuple; the comma is required
let unit = ()               // the empty tuple
```

`(5)` is a parenthesised expression, not a tuple - the trailing comma in `(5,)`
is what makes it one.

## Positional access

Elements are read with `.0`, `.1`, `.2`, and so on. Reads chain through nested
tuples:

```gossamer
let nested = ((1, 2), "outer")
println!("{}", nested.0.1)   // 2
```

A `mut` binding assigns positionally, including through a nested tuple:

```gossamer
let mut counter = (0, "hits")
counter.0 = 7
let mut grid = ((1, 2), 3)
grid.0.1 = 42
```

## Destructuring

A tuple pattern binds every element at once, in `let`, in `for`, in `match`, and
in a function's parameter list:

```gossamer
let (id, name, weight) = (1, "two", 3.0)

for (key, value) in map.iter() {
    println!("{key}={value}")
}

fn label((rank, name): (i64, String)) -> String {
    format!("{rank}: {name}")
}

match point {
    (0, 0) => println!("origin"),
    (x, _) => println!("x = {x}"),
}
```

## Comparison and ordering

Tuples compare structurally, element by element in declaration order, with no
`#[derive(...)]`. Equality needs every element equal; ordering is
lexicographic, so the first differing element decides.

```gossamer
println!("{}", (1, 2) < (1, 3))     // true
println!("{}", (1, "a") == (1, "a")) // true
```

That ordering is what `sort` uses on a sequence of tuples, which makes a tuple
the usual sort key:

```gossamer
let mut pairs = [(3, "c"), (1, "a"), (2, "b")]
pairs.sort()
```

## Where tuples appear

A tuple is an ordinary value: it can be a function return, a struct field, a
`Vec` element, a `Map` key, or a channel payload.

```gossamer
fn min_max(xs: &[i64]) -> (i64, i64) {
    (xs.min().unwrap_or(0), xs.max().unwrap_or(0))
}

struct Reading { at: (i64, i64), value: f64 }

let by_position: Map<(i64, i64), String> = Map::new()
```

`Map::iter()` yields `[(K, V)]`, and `Vec::enumerate()` yields
`Vec<(i64, T)>`, so the `for (a, b) in ...` shape reads the same everywhere.

## Discovery

`%info Tuple` in the REPL describes the type, and `%explain <binding>` on a
tuple binding lists its positional elements and their types:

```text
>>> let t = (1, "two", 3.0)
>>> %e t
t: (i64, String, f64) [binding]
t.0: i64 [element]
t.1: String [element]
t.2: f64 [element]
```
