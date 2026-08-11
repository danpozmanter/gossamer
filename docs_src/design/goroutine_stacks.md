# Goroutine stack model

Every goroutine runs on its own stackful coroutine
([`gossamer-coro`](https://docs.rs/corosensei) over `corosensei`),
scheduled M:N across a small pool of worker threads. This note explains
how those stacks are sized and grown, and why Gossamer uses
**guard-page / lazy-commit** stacks rather than Go's **copyable** ones.

## How a goroutine stack works

A new goroutine reserves a fixed range of virtual address space (1 MiB
by default; override with `GOSSAMER_GOROUTINE_STACK=N`). The range is an
anonymous `mmap` with a `PROT_NONE` guard page at the low end. Two
properties follow from that single decision:

- **Lazy commit.** The operating system does not back the reservation
  with physical pages until the goroutine actually touches them. A
  goroutine that only ever uses a few hundred bytes of stack costs a few
  hundred bytes of resident memory, even though its reservation is
  1 MiB. Resident memory tracks real depth, not the reservation.
- **The stack never moves.** The reservation is chosen once, at
  allocation, and its base address is stable for the goroutine's whole
  life. Nothing ever relocates a live stack.

A byte-budget recursion guard is armed at the shallow entry point of
each goroutine (and re-armed on every resume, since a goroutine can
migrate between worker threads). The bytecode VM consults it on every
call, so runaway recursion there trips a clean `GX0008` diagnostic
before it can reach the hardware guard page.

Compiled frames do not consult it: neither backend emits a prologue
safepoint. JIT-compiled and native recursion instead reaches the guard
page, where the installed handler reports the same `GX0008` code and
exits with the same status the VM uses, naming the faulting address it
alone knows. Both paths are deterministic and neither corrupts memory.

Checking the budget in compiled prologues was measured and rejected: an
opaque runtime call at every function entry blocks inlining, and a
call-heavy benchmark (recursive `fib(40)`, `--release`) went from 0.16 s
to 0.96 s. The guard page costs nothing until it is hit.

Because the reservation is generous and commit is lazy, both cheap
goroutines and deep recursion fall out of the same mechanism: thousands
of concurrent goroutines stay in the tens of MiB of resident memory,
while a deeply recursive function (a recursive descent parser, a
tree walk) runs to a depth bounded only by the reservation, not by a
small initial stack.

## Why not copyable stacks (Go's approach)

Go starts each goroutine with a tiny (KB-scale) stack and **grows it by
copying**: when a function prologue detects the stack is about to
overflow, the runtime allocates a larger stack, copies the live frames
into it, and rewrites every pointer that referred into the old stack.
This gives Go the best-in-class combination of tiny initial stacks and
unbounded growth, and the ability to shrink stacks back down.

That power has a price Gossamer would have to pay in full, and the
price is much higher here than in Go:

- **Precise stack maps.** Copying a stack means finding and rewriting
  every pointer into it. That requires the compiler to emit precise
  stack maps (which slots hold pointers at every safepoint) for both the
  Cranelift and LLVM backends. Gossamer manages memory with reference
  counting plus arenas, not a tracing garbage collector, so it has
  **no existing stack-map infrastructure to reuse** - it would be built
  from scratch, twice.
- **Native FFI actively fights stack relocation.** Gossamer has
  first-class native bindings (`[rust-bindings]`, the `gos_rt_*` ABI).
  If a goroutine calls into native code that holds a pointer into the
  Gossamer stack, relocating that stack under the call corrupts it. Go
  survives this by switching to a system stack for the duration of a
  cgo call and never moving stacks mid-FFI; Gossamer would have to
  replicate that escape hatch, and it would interact with the M:N
  scheduler and the coroutine stacks in subtle ways.
- **Three tiers, one meaning.** A feature must behave identically on the
  bytecode VM, the Cranelift JIT, and the LLVM AOT backend. The VM does
  not use the machine stack for Gossamer frames the way native code does
  (its frames are heap-backed, and deep recursion there is a clean
  depth-guarded `GX0008` rather than a hardware overflow). "Grow the stack by
  copying frames" is three different problems across the three tiers,
  and unifying them is awkward.

Guard-page / lazy-commit stacks sidestep all three: no stack maps, no
FFI pinning problem (the stack never moves), and the same reservation
model works uniformly under every tier.

## Tradeoffs

| Property | Copyable stacks (Go) | Guard-page growth (Gossamer) |
|---|---|---|
| Cheap idle goroutines | Best (KB-scale minimum) | Good (small committed floor) |
| Deep recursion | Yes (grows unbounded) | Yes (up to the reservation) |
| Stack maps required | Yes (large, per-backend) | No |
| Safe with native FFI | Needs a system-stack escape hatch | Yes (stack never moves) |
| Can shrink / maximum density | Yes | No (address space held, pages lazy) |
| Effort and risk to build | Very high | Low to moderate |

The one column Gossamer gives up is the last-but-one: a guard-page stack
cannot shrink, and its density is bounded by **address space** rather
than by physical memory. On a 64-bit host address space is abundant, so
this only becomes a first-order constraint at extreme goroutine counts
(tens of millions of simultaneously live goroutines) or in a very tight
embedded memory budget. In those regimes copyable stacks pull ahead -
and even there, the native-FFI escape hatch would still be owed.

## When this decision would be revisited

Copyable stacks become the right investment only if goroutine density at
the 10-million-plus scale, or resident memory per idle goroutine in an
embedded deployment, becomes a first-order goal - and only alongside a
willingness to build precise stack maps across both compiled backends
and a system-stack escape hatch for native calls. Until then, guard-page
on-demand growth is the Go-competitive path that fits Gossamer's
reference-counted, FFI-first, tri-tier design.
