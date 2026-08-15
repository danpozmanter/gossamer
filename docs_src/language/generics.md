# `lang::generics`

Type parameters on functions / impls / structs.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## Trait bounds and static dispatch

A generic function may bound a type parameter by a trait and call that
trait's methods on a parameter receiver:

```gossamer
trait Shape {
    fn name(&self) -> String
    fn area(&self) -> i64
}

fn report<T: Shape>(s: &T) -> String {
    format!("{}: {}", s.name(), s.area())
}
```

- Each call site instantiates the type parameters independently, so one
  generic function serves any number of concrete types in a program.
- The bound is enforced: passing a type with no matching `impl` is a
  compile error (`GT0017`).
- A method called on a bound parameter resolves to the trait method's
  declared return type.
- Every instantiation is monomorphised and the trait-method call lowers
  to the concrete impl symbol (`Square::name`), giving static dispatch
  that is bit-identical across the VM, Cranelift, and LLVM tiers.
- A bound may pin an associated type with an equality constraint
  (`T: Holder<Item = i64>`), and `T::Item` / `T::MAX` project the bound
  trait's associated type / constant - see
  [trait](trait.md#associated-types).

Supported today: type parameters with one or more bounds (`T: A + B`),
written in the parameter list or a `where` clause, with struct arguments
and inherent static dispatch. Not yet part of static dispatch: `dyn Trait`,
blanket impls, and supertrait method inheritance through a bound.

## Generic struct types

A struct may hold its type parameter by value, and methods on it use the
`impl<T>` form (each receiver type specialises the method, so `-> T`
returns the real instantiated type):

```gossamer
struct Wrapper<T> { value: T }

impl<T> Wrapper<T> {
    fn get(&self) -> T { self.value }
}

fn main() {
    let n = Wrapper { value: 42 }
    let s = Wrapper { value: "hi" }
    println!("{} {}", n.get(), s.get())   // 42 hi
}
```

Each instantiation lays the field out by its concrete type (a
`Wrapper<Point>` stores a whole `Point` inline) and runs bit-identically
on every tier. Multiple type parameters (`Pair<A, B>`), nested generic
structs, and arrays of generic structs all work.

## Const-generic array length

A function may take a fixed-size array of generic length:

```gossamer
fn sum<const N: usize>(xs: [i64; N]) -> i64 {
    let mut acc = 0
    for x in xs { acc += x }
    acc
}

fn main() {
    println!("{} {}", sum([1, 2, 3]), sum([10, 20, 30, 40, 50]))  // 6 150
}
```

- `N` is inferred from the array argument's length at the call site and
  keyed into monomorphisation, so each distinct length is its own
  specialisation.
- The function body may iterate the parameter and read `xs.len()`, the
  const may appear in the return type (`-> [i64; N]`), and a function may
  take more than one const parameter (`<const N: usize, const M: usize>`).
- Every instantiation runs bit-identically across the bytecode VM, the
  Cranelift JIT, and the LLVM AOT tiers.

Supported today: the const is inferred from a `[T; N]` argument's length.
Not yet supported: using `N` as a bare value expression in the body or as
a repeat count (`[0; N]`).
