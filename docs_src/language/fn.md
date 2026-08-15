# `lang::fn`

Function declaration.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

`fn name(params) -> Ret { body }`. The last expression is the return
value (no `return` needed); `-> Ret` may be omitted when the function
returns `()`. A parameter takes `name: Type`, `&Type` (read-shared), or
`&mut Type` (writes through to the caller).

Mutable-reference arguments must be explicit at the call site. A writable
binding is passed as `&mut value`, while an existing `&mut Type` reference can
be forwarded as-is. Passing a bare value never grants a callee mutable access.

```gossamer
fn area(w: i64, h: i64) -> i64 { w * h }
fn greet(name: &String) { println!("hi, {name}") }
```

## Parameter pattern destructuring

A parameter may be a pattern instead of a plain name - tuple or struct - and
binds its components directly:

```gossamer
struct Point { x: i64, y: i64 }
struct Pair { left: i64, right: i64 }

fn dot((a, b): (i64, i64)) -> i64 { a * b }
fn sum(Point { x, y }: Point) -> i64 { x + y }
fn diff(Pair { left, right }: Pair) -> i64 { left - right }
```

## Generics

A function may take type parameters (`fn id<T>(x: T) -> T`), a trait bound
(`fn report<T: Shape>(s: &T)`), or a const-generic array length
(`fn sum<const N: usize>(xs: [i64; N])`). Each instantiation is
monomorphised - see [generics](generics.md).
