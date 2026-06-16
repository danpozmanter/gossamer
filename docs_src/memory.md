# Memory model

Gossamer manages memory for you automatically: there is no borrow
checker, no lifetime annotations, and no manual ownership transfer.
`&` and `&mut` exist - but they express *aliasing intent*, not
ownership.

Under the hood the compiled tiers use **deterministic reference
counting** with a cycle collector, not a tracing collector: most
values are reclaimed at the moment their last reference dies, RAM
stays flat and predictable, and there are no pause times. The
`arena { }` block (below) layers bulk allocation on top for
short-lived object graphs.

## Values vs references

- **Value-semantic types** are copied on assignment and passed
  by value: `bool`, `char`, `i8`..`i64`, `u8`..`u64`,
  `isize`/`usize`, `f32`/`f64`.
- **Reference-semantic types** share their backing storage when
  copied; the runtime reclaims the backing when the last reference
  dies. This includes `String`, `Vec<T>`, `[T]`, `struct`, `enum`,
  closures.

## `&` and `&mut`

`&x` means "read `x` without taking ownership". `&mut x` means
"write `x` without taking ownership". The type checker rejects:

- A `&mut` overlapping with another `&` or `&mut` in a visible
  scope.
- A `&mut` taken on a non-`mut` binding.

These are *correctness* rules, not lifetime proofs. You never
write `'a`.

## How reclamation works

- **Reference counting, compiler-inserted.** The MIR lowering inserts
  balanced retain/release pairs; a liveness pass releases owned values
  at their *last use* rather than at function exit, so a large
  structure does not pin memory while unrelated code runs.
- **Cycle collector.** Reference cycles (`a.next = Some(b);
  b.next = Some(a)`) are reclaimed by a Bacon-Rajan style cycle
  collector that runs on demand (`runtime::collect_cycles()`) and from
  allocation pressure. Acyclic data never pays for it.
- **Weak references.** `Weak<T>` observes a value without keeping it
  alive; `w.upgrade()` returns `Option<T>` and answers `None` once the
  referent has been reclaimed.
- **Compact representation.** A heap enum node carries an 8-byte
  runtime header. Enums with at most 4 variants store their
  discriminant in pointer tag bits, so a two-pointer tree node costs
  24 bytes - and only 16 inside an arena.

## Arenas: `arena { }`

An `arena` block bump-allocates everything created while it runs and
frees the whole lot at once when the block exits:

```gossamer
fn main() {
    let mut total = 0
    let mut i = 0
    while i < 1000 {
        arena {
            let tree = build_tree(16)
            total += check(&tree)
        }
        i += 1
    }
    println!("{}", total)
}
```

Semantics:

- **Allocation is a pointer bump** - a compare and an add, roughly an
  order of magnitude cheaper than a general heap allocation.
- **Reclamation is wholesale** - the arena's slabs are released in
  O(slabs) when the block exits, with no per-object teardown walk. The
  exit is exact on every path: early `return`, `?`, `break`, and
  normal fall-through all release the arena (the block desugars to a
  `defer`).
- **Headerless nodes.** Enum nodes of tagged-repr types (at most 4
  variants) allocated inside an arena carry **no header at all**: a
  `Node(Box<Tree>, Box<Tree>)` is exactly 16 bytes.
- **Arenas nest.** An inner `arena { }` frees at its own close brace;
  the outer arena is unaffected. Slabs from finished arenas are
  recycled, so an arena per loop iteration costs a bump-pointer reset,
  not an `mmap`.
- **Retain/release become no-ops** for arena values - the accounting
  entries recognize arena memory with a two-instruction range check.

### The contract

Nothing allocated inside an `arena { }` may be referenced after the
block exits. The block is statement-position only and yields unit, so
the obvious escape (the block's value) cannot happen, and a tail
expression inside it is deliberately discarded. The remaining ways to
leak a pointer out are yours to avoid:

- assigning an arena value to a binding declared **outside** the block,
- pushing arena values into a container that outlives the block,
- sending arena values down a channel,
- capturing them in a closure or goroutine that outruns the block.

Use an arena when the block is *pure computation over data that dies
together* - build, traverse, summarize, exit. If a value must survive,
let the block compute a scalar/string summary, or build the surviving
value before (outside) the arena.

Two deliberate edge-case rules: `Weak` references to arena values
upgrade to `None` (the referent is not individually tracked), and a
panic that unwinds out of a goroutine mid-arena abandons the arena's
slabs to the goroutine's teardown rather than corrupting them.

`runtime::arena_push()` / `runtime::arena_pop()` remain available as
the low-level primitive when block structure does not fit; the block
form is the idiom because it cannot be left unbalanced.

## Goroutine stacks

Each `go expr` launches a goroutine with its own stack. Captures are
reference-counted exactly as regular struct fields would be.

## When to reach for `Rc<RefCell<T>>`-like patterns

You generally don't. Shared aliasing works directly. If you need to
mutate through a shared handle across goroutines, hold the value in a
`Mutex<T>` (from `std::sync`) and lock around every mutation.

## Stack vs heap - the pragmatic answer

- Small value types live on the stack or inline inside their
  aggregate.
- Aggregates (`String`, `Vec<T>`, structs, enums, closures) live on
  the heap, reference-counted; `Box<T>` / `Rc<T>` / `Arc<T>` spellings
  are accepted and transparent.
- Short-lived object graphs belong in an `arena { }`.
