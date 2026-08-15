# `lang::callback_shorthand`

A callback written without `|v|`: a std free function named in value position, and a `$`-headed projection argument (`xs.map($.abs)`), both stand for the closure that calls them.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

A callback slot takes a closure. When that closure would do nothing but
call one function on its parameter, two shorthands write the call
directly.

## A std function in value position

Naming a std free function where a callback is expected takes the closure
that calls it:

```gossamer
use std::{encoding::base64, math}

fn main() {
    println!("{:?}", #[1.0, -2.0].map(math::abs))
    println!("{:?}", #["ab", "cd"].map(base64::encode))
}
```

Each of those is the closure `|v| math::abs(v)` / `|v| base64::encode(v)`.
The parameter count comes from the function's own signature, so a function
with no fixed parameter list - `fmt::format`, which is variadic - has no
closure to stand for and reports `GT0015`. Write that one out:

```gossamer
use std::fmt

fn main() {
    println!("{:?}", #[1, 2].map(|v| fmt::format("<{}>", v)))
}
```

## A `$`-headed projection

A `$` projection written as a call argument is the closure over that
argument, with the forms `$` already has in a pipe step:

```gossamer
fn main() {
    println!("{:?}", #[1.0, -2.0].map($.abs))
    println!("{:?}", #["a", "bb"].map($.len()))
    println!("{:?}", #[(1, 2), (3, 4)].map($.0))
}
```

`$.name` is the nullary method call, exactly as in a pipe step, so it does
not read a struct field. A field projection stays a written closure:

```gossamer
struct Person { name: String }

fn main() {
    let people = #[Person { name: "ada" }, Person { name: "grace" }]
    println!("{:?}", people.map(|p| p.name))
}
```

A bare `$` argument keeps its pipe meaning - it selects the slot the piped
value lands in - so `x |> f($, k)` is `f(x, k)`, not a callback.
