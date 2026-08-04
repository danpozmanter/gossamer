# Collection literals

Gossamer has dedicated literal forms for the everyday collection shapes:
growable vectors, fixed arrays, hash maps, hash sets, ordered sets, queues,
and heaps.

```gos
let values = [1, 2, 3]
let fixed = #[1, 2, 3]
let names = {"ada": 36, "grace": 37}
let tags = #{"compiler", "runtime", "docs"}
let ordered: BTreeSet<String> = #{"compiler", "runtime", "docs"}
let queue = <[1, 2, 3]>
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

## HashMap

A brace literal creates a `HashMap<K, V>`.

```gos
let ages = {"ada": 36, "grace": 37}
println(ages.get("ada"))
```

The empty brace literal creates an empty `HashMap`.

```gos
let counts = {}
println(counts.len())

let typed: HashMap<String, i64> = {}
println(typed.len())
```

Annotate an empty map when later code does not give the checker enough key and
value information.

## HashSet And BTreeSet

Use `#{...}` for a `HashSet<T>`.

```gos
let seen = #{"ada", "grace", "ada"}
println(seen.len())       // 2
println(seen.contains("ada"))
```

The literal removes duplicates just like repeated `insert` calls.

An expected `BTreeSet<T>` type shapes the same literal into an ordered set:

```gos
let ordered: BTreeSet<String> = #{"grace", "ada", "ada"}
println(ordered.to_vec())
```

## VecQueue and VecDeque

Use `<[...]>` for a queue literal. Phase 1 queue literals create
`VecDeque<i64>` in front-to-back order. `VecQueue<i64>` and
`VecDequeue<i64>` are aliases for the same runtime shape.

```gos
let mut q: VecQueue<i64> = <[10, 20]>
q.push_back(30)
println(q.pop_front())
```

## MaxHeap and MinHeap

Use `^[...]` for a `MaxHeap<i64>` and `_[...]` for a `MinHeap<i64>`.
`BinaryHeap<i64>` is accepted as a compatibility alias for `MaxHeap<i64>`.

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
| `{}` | empty `HashMap<K, V>` |
| `{key: value}` | `HashMap<K, V>` |
| `#{a, b}` | `HashSet<T>`, or `BTreeSet<T>` with an expected type |
| `<[a, b]>` | `VecDeque<i64>` |
| `^[a, b]` | `MaxHeap<i64>` |
| `_[a, b]` | `MinHeap<i64>` |
