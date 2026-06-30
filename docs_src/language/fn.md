# `lang::fn`

Status: shipped

Function declaration.
<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

`fn name(params) -> Ret { body }`. The last expression is the return
value (no `return` needed); `-> Ret` may be omitted when the function
returns `()`. A parameter takes `name: Type`, `&Type` (read-shared), or
`&mut Type` (writes through to the caller).

```gossamer
fn area(w: i64, h: i64) -> i64 { w * h }
fn greet(name: &String) { println!("hi, {name}") }
```

## Parameter pattern destructuring

A parameter may be a pattern instead of a plain name - tuple, struct, or
tuple-struct - and binds its components directly:

```gossamer
struct Point { x: i64, y: i64 }
struct Pair(i64, i64)

fn dot((a, b): (i64, i64)) -> i64 { a * b }
fn sum(Point { x, y }: Point) -> i64 { x + y }
fn diff(Pair(a, b): Pair) -> i64 { a - b }
```

## Generics

A function may take type parameters (`fn id<T>(x: T) -> T`), a trait bound
(`fn report<T: Shape>(s: &T)`), or a const-generic array length
(`fn sum<const N: usize>(xs: [i64; N])`). Each instantiation is
monomorphised - see [generics](generics.md).
