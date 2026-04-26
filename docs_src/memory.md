# Memory model

Gossamer is garbage-collected. There is no borrow checker, no
lifetime annotations, and no manual ownership transfer. `&` and
`&mut` still exist — but they express *aliasing intent*, not
ownership.

## Values vs references

- **Value-semantic types** are copied on assignment and passed
  by value: `bool`, `char`, `i8`..`i128`, `u8`..`u128`,
  `isize`/`usize`, `f32`/`f64`.
- **Reference-semantic types** share their backing storage when
  cloned. The GC reclaims the backing when no root references it.
  This includes `String`, `Vec<T>`, `[T]`, `struct`, `enum`,
  closures.

## `&` and `&mut`

`&x` means "read `x` without taking ownership". `&mut x` means
"write `x` without taking ownership". The type checker rejects:

- A `&mut` overlapping with another `&` or `&mut` in a visible
  scope.
- A `&mut` taken on a non-`mut` binding.

These are *correctness* rules, not lifetime proofs. You never
write `'a`.

## The garbage collector

Phase-14 GC is a stop-the-world mark-sweep. Upcoming phases
layer on:

- **Concurrent marking** — `Heap::concurrent_start` +
  `concurrent_step` lets the mutator run while the grey-set is
  drained incrementally.
- **Pause histogram** — `GcStats.{last_pause_nanos,
  total_pause_nanos, max_pause_nanos}` are populated on every
  `Heap::collect` call. See
  [`docs/perf_baseline.md`](design/perf_baseline.md) for
  reference numbers.
- **Weak references** — `gossamer_gc::weak::WeakTable` lets code
  observe an allocation without rooting it.
- **Finalisers** — `FinalizerSet::register(handle, callback)`
  runs a cleanup closure when the target is swept. Use for OS
  handles (files, sockets, listeners).

## Goroutine stacks

Each `go expr` launches a goroutine with its own stack (fixed-
size for the moment — growable stacks land with the Stream E.4
scheduler rewrite). Captures are reference-counted into the GC
heap exactly as regular struct fields would be.

## When to reach for `Rc<RefCell<T>>`-like patterns

You generally don't. The GC rescues most single-threaded
aliasing. If you need to mutate through a shared handle, hold a
`struct` inside a `Mutex<T>` (from `std::sync`) and lock around
every mutation.

## Stack vs heap — the pragmatic answer

- Small value types live on the stack or inline inside their
  aggregate.
- Aggregates (`String`, `Vec<T>`, structs, closures) live on the
  GC heap. There is no syntactic `Box<T>`.
- An [escape analysis][escape] pass (Stream E.6) has landed in
  MIR and can demote short-lived allocations to the stack once
  the codegen picks them up.

[escape]: https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-mir/src/escape.rs
