# `lang::generics`

Status: shipped

Type parameters on functions / impls / structs.
<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## Trait bounds and static dispatch

A generic function may bound a type parameter by a trait and call that
trait's methods on a parameter receiver:

```gossamer
trait Shape { fn name(&self) -> String; fn area(&self) -> i64; }

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

Supported today: single-bound type parameters with struct arguments and
inherent static dispatch. Not yet part of static dispatch: `dyn Trait`,
operator traits, associated-type projection in bounds, blanket impls, and
supertrait method inheritance through a bound.

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
