# Collection literals

Gossamer has dedicated literal forms for the everyday collection shapes:
growable vectors, fixed arrays, hash maps, hash sets, ordered sets, queues,
stacks, and heaps.

```gos
let values = [1, 2, 3]
let fixed = #[1, 2, 3]
let names = {"ada": 36, "grace": 37}
let tags = #{"compiler", "runtime", "docs"}
let ordered: BTreeSet<String> = #{"compiler", "runtime", "docs"}
let queue = <[1, 2, 3]
let stack = [1, 2, 3]>
let max_heap = ^[1, 2, 3]
let min_heap = _[1, 2, 3]
```

## Vec

A plain bracket literal creates a `Vec<T>` unless an expected fixed-array type
shapes it.

```gos
let scores = [10, 20, 30]
let mut queue = []
queue.push(40)
queue.push(50)
```

Use `Vec::with_capacity(n)` when capacity matters before pushing:

```gos
let mut bytes = Vec::<u8>::with_capacity(1024)
bytes.push(65)
```

## Fixed Arrays

Use `#[...]` when the value must be an owned fixed-size array.

```gos
let point = #[3, 4]
let zeros = #[0; 4]
```

An expected `[T; N]` type can also shape a plain bracket literal:

```gos
let point: [i64; 2] = [3, 4]
```

Fixed arrays and slices support non-resizing sequence methods. Use a `Vec<T>`
when the collection must grow or shrink.

## Map

A brace literal creates a `Map<K, V>`.

```gos
let ages = {"ada": 36, "grace": 37}
println(ages.get("ada"))
```

The empty brace literal creates an empty `Map`.

```gos
let counts = {}
println(counts.len())

let typed: Map<String, i64> = {}
println(typed.len())
```

Annotate an empty map when later code does not give the checker enough key and
value information.

`HashMap` remains accepted as a longer alias for `Map`.

## Set And BTreeSet

Use `#{...}` for a `Set<T>`.

```gos
let seen = #{"ada", "grace", "ada"}
println(seen.len())       // 2
println(seen.contains("ada"))
```

The literal removes duplicates just like repeated `insert` calls.

`HashSet` remains accepted as a longer alias for `Set`.

An expected `BTreeSet<T>` type shapes the same literal into an ordered set:

```gos
let ordered: BTreeSet<String> = #{"grace", "ada", "ada"}
println(ordered.to_vec())
```

## Queue

Use `<[...]` for a FIFO queue literal. Phase 1 queue literals create
`Queue<i64>` in front-to-back order. `push` appends to the back and
`pop` removes from the front. Use `peek`, `len`, `is_empty`, and `clear` for
the common queue observers and reset operation.

```gos
let mut q: Queue<i64> = <[10, 20]
q.push(30)
println(q.len())
println(q.peek())
println(q.pop())
```

`VecQueue` remains accepted as a longer alias for `Queue`.

## Stack

Use `[a, b]>` for a LIFO stack literal. Phase 1 stack literals create
`Stack<i64>` in bottom-to-top order. `push` appends to the top and
`pop` removes from the top. Use `peek`, `len`, `is_empty`, and `clear` for
the common stack observers and reset operation.

```gos
let mut s: Stack<i64> = [10, 20]>
s.push(30)
println(s.len())
println(s.peek())
println(s.pop())
```

`VecStack` remains accepted as a longer alias for `Stack`.

## Deque

Use `Deque<i64>` when both ends matter. It has explicit front/back methods
and no dedicated literal.

```gos
let mut d: Deque<i64> = Deque::from([10, 20])
d.push_front(5)
d.push_back(30)
println(d.pop_front())
println(d.pop_back())
```

`VecDeque` remains accepted as a longer alias for `Deque`.

## MaxHeap and MinHeap

Use `^[...]` for a `MaxHeap<i64>` and `_[...]` for a `MinHeap<i64>`.
`BinaryHeap<i64>` and `MaxBinaryHeap<i64>` are accepted as compatibility
aliases for `MaxHeap<i64>`. `MinBinaryHeap<i64>` is accepted as a longer alias
for `MinHeap<i64>`.

```gos
let mut max_heap = ^[5, 1, 3]
println(max_heap.peek())  // Some(5)
max_heap.push(8)
println(max_heap.pop())   // Some(8)

let mut min_heap = _[5, 1, 3]
println(min_heap.peek())  // Some(1)
min_heap.push(0)
println(min_heap.pop())   // Some(0)
```

## Summary

| Literal | Result |
|---|---|
| `[a, b]` | `Vec<T>` |
| `#[a, b]` | `[T; N]` fixed array |
| `#[value; count]` | repeated fixed array |
| `{}` | empty `Map<K, V>` |
| `{key: value}` | `Map<K, V>` |
| `#{a, b}` | `Set<T>`, or `BTreeSet<T>` with an expected type |
| `<[a, b]` | `Queue<i64>` |
| `[a, b]>` | `Stack<i64>` |
| `^[a, b]` | `MaxHeap<i64>` |
| `_[a, b]` | `MinHeap<i64>` |
