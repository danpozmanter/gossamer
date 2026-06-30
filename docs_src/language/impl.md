# `lang::impl`

Status: shipped

Inherent and trait implementation blocks.
<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

An inherent `impl Type` block adds methods and associated functions; an
`impl Trait for Type` block supplies a trait's methods.

```gossamer
struct Point { x: i64, y: i64 }

impl Point {
    fn origin() -> Point { Point { x: 0, y: 0 } }   // associated fn
    fn norm(&self) -> i64 { self.x * self.x + self.y * self.y }
}

let p = Point::origin()   // qualified path always resolves
println!("{}", p.norm())
```

A method takes `self`, `&self`, or `&mut self`; `&self` reads, `&mut self`
writes through to the caller's storage. Qualified-path calls
(`Point::origin()`) always resolve; bare-name method dispatch is
name-global in places, so prefer the qualified form when a name could
collide.

## Generic impls

Methods on a generic struct use the `impl<T>` form, and each receiver type
specialises the method (so `-> T` returns the real instantiated type):

```gossamer
struct Wrapper<T> { value: T }

impl<T> Wrapper<T> {
    fn get(&self) -> T { self.value }
}
```

## Operator and conversion impls

Operator overloading (`impl Add for T`, `impl Index for T`, ...) and
conversions (`impl T { fn from(...) }`, `fn try_from(...)`) are ordinary
impls - see [trait](trait.md) for the operator/method table and the
`into` / `try_into` inference rules. The comparison (`eq` / `cmp`) and
`clone` behaviour is synthesized with no impl; write an `eq` / `cmp` impl
only to override the default ordering.
