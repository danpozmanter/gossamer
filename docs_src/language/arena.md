# `arena { }`

An `arena` block gives a span of code its own bump allocator:
everything allocated while the block runs lands in the arena, and the
whole arena is freed at once when the block exits.

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

## Why

For object graphs that die together — a parse tree, a request's
working set, a per-iteration data structure — individual reference
counting does work the program does not need. Inside an arena:

- **allocation is a pointer bump** (compare + add);
- **reclamation is wholesale**: slabs are released in O(slabs), with
  no per-object walk;
- **small-enum nodes are headerless**: an enum with at most 4 variants
  keeps its discriminant in pointer tag bits, so
  `Node(Box<Tree>, Box<Tree>)` costs exactly 16 bytes;
- **retain/release are no-ops** for arena values (a two-instruction
  range check at the accounting entries).

## Exit behavior

The block desugars to `runtime::arena_push()` plus a block-scoped
`defer runtime::arena_pop()`, so the arena is released on **every**
exit path: normal fall-through, early `return`, `?` propagation, and
`break`/`continue` out of the block.

Arenas nest: an inner `arena { }` frees at its own close brace without
touching the outer one. Slabs from finished arenas are recycled, so an
arena per loop iteration is a bump-pointer reset, not a fresh `mmap`.

## The contract

Nothing allocated inside the block may be referenced after it exits.
The block is statement-position only and yields unit (a tail
expression is discarded), which rules out the obvious escape. The
remaining escapes are yours to avoid: assigning to an outer binding,
pushing into a container that outlives the block, sending down a
channel, or capturing in a goroutine/closure that outruns the block.

Compute summaries inside, keep survivors outside:

```gossamer
let mut best = 0
arena {
    let g = build_graph(n)
    best = score(&g)      // scalar out: fine
}
// `g` is gone; `best` survives.
```

Edge cases, pinned: `Weak` references to arena values upgrade to
`None`; unit-variant singletons (`Tree::Nil`) are process-immortal and
safe to reference anywhere.

## The primitive

`runtime::arena_push()` / `runtime::arena_pop()` are the underlying
calls for shapes where block structure does not fit. Prefer the block:
it cannot be left unbalanced.
