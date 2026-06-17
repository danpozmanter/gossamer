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
