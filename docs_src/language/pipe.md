# `lang::pipe`

Forward-pipe operator `|>`, for composing free functions in a functional style. A step is either a bare callable (`x |> f`) or a call that names the piped value's slot with `$` (`x |> f(a, $)`). Methods chain on their own and are the shorter spelling; a method chain can feed a pipe.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## What the pipe is for

A free function has no receiver, so a chain of them nests inside-out and
reads right to left. `|>` turns that back into reading order:

```gossamer
struct Order { id: i64, total: f64 }

fn with_tax(o: Order) -> Order { Order { id: o.id, total: o.total * 1.2 } }
fn discount(pct: f64, o: Order) -> Order { Order { id: o.id, total: o.total * (1.0 - pct) } }
fn label(o: Order) -> String { format!("#{} {}", o.id, o.total) }

fn main() {
    let o = Order { id: 1, total: 100.0 }
    println!("{}", o |> with_tax |> discount(0.1, $) |> label)
}
```

Without the pipe that is `label(discount(0.1, with_tax(o)))`, which the
reader has to unwrap from the inside.

## Prefer methods where a receiver exists

Anything with a receiver already chains, and the chain is shorter. Reach
for the pipe when the transforms are free functions, not to restate a
method call:

```gossamer
fn main() {
    // write this
    println!("{}", "  Ab  ".trim().to_lowercase())

    // not a pipe form of the same thing
}
```

The two mix: a method chain is an ordinary operand, so it can feed a pipe
step, and a pipe step's result can be chained onto.

```gossamer
fn exclaim(s: String) -> String { s + "!" }

fn main() {
    println!("{}", "  Ab  ".trim().to_lowercase() |> exclaim)
}
```

## The two step shapes

A **bare callable** takes the piped value as its only argument:

```gossamer
fn double(n: i64) -> i64 { n * 2 }

fn main() {
    println!("{}", 3 |> double)
}
```

A step that **writes arguments must name the slot** with `$`. Exactly one
`$` per step, and it may sit anywhere along the step's call chain:

```gossamer
use std::{iter, strings}

fn main() {
    println!("{:?}", "a,b,c" |> strings::split($, ","))
    println!("{}", #[1, 2, 3, 4] |> iter::filter(|x| x % 2 == 0, $).len())
}
```

A closure step takes the value as the closure's parameter:

```gossamer
fn main() {
    println!("{}", 3 |> |v| v * 2)
}
```

## Why the slot is named

Gossamer's free functions do not share one argument convention. `iter::`,
`option::`, and `result::` take their data last; `strings::`, `bytes::`,
`path::`, `sort::`, and `fs::` take it first, mirroring the method
receiver. An operator that assumed one convention would silently mis-fill
the other, and those signatures are homogeneous enough that the type
checker could not catch it - `strings::split(String, String)` accepts the
arguments either way round.

Naming the slot removes the assumption. Both conventions read the same,
and the reader can see which argument the value fills:

```gossamer
use std::{iter, strings}

fn main() {
    println!("{:?}", "a,b,c" |> strings::split($, ","))   // data first
    println!("{:?}", #[1, 2] |> iter::map(|v| v * 2, $))  // data last
}
```

Omitting `$` on an argument-taking step reports `GP0041`.

## What `$` is not

`$` names a pipe slot and nothing else.

- It is not a callback shorthand: `xs.map($.abs)` reports `GP0043`. Write
  `xs.map(math::abs)` or `xs.map(|v| v.abs())`.
- It does not paste a receiver back on: `x |> $.trim` reports `GP0042`.
  Write `x.trim()`.
- `x |> $` reports `GP0042` as well - the identity step is the value.

`gos check --fix` rewrites all three. On `GP0041` it appends `$` in the
trailing slot, which preserves the behaviour of the implicit rule it
replaces, so confirm that is the slot the call needs.
