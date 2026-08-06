# Collection literals

Gossamer has dedicated literal forms for the everyday collection shapes:
growable vectors (`#[...]`), fixed arrays (`[...]`), maps, and sets. The remaining containers -
queues, stacks, deques, and heaps - are built through their type.

```gos
use std::collections::{Queue, Stack, Deque, MaxHeap, MinHeap}

let values = #[1, 2, 3]
let fixed = [1, 2, 3]
let names = {"ada": 36, "grace": 37}
let tags = #{"compiler", "runtime", "docs"}
let ordered: BTreeSet<String> = #{"compiler", "runtime", "docs"}
let queue = Queue::from([1, 2, 3])
let stack = Stack::from([1, 2, 3])
let deque = Deque::from([1, 2, 3])
let max_heap = MaxHeap::from([1, 2, 3])
let min_heap = MinHeap::from([1, 2, 3])
```

Every container has exactly one name. `HashMap`, `HashSet`, `VecDeque`,
`VecQueue`, `VecStack`, `BinaryHeap`, `MaxBinaryHeap`, and `MinBinaryHeap` are
not accepted; write `Map`, `Set`, `Deque`, `Queue`, `Stack`, `MaxHeap`, and
`MinHeap`.

## Vec

`#[...]` creates a `Vec<T>` - the default growable sequence - unless an
expected fixed-array type shapes it.

```gos
let scores = #[10, 20, 30]
let mut pending = #[]
pending.push(40)
pending.push(50)
```

Use `Vec::with_capacity(n)` when capacity matters before pushing:

```gos
let mut bytes = Vec::<u8>::with_capacity(1024)
bytes.push(65)
```

## Fixed Arrays

`[...]` creates an owned fixed-size array `[T; N]`, whose length is part of its
type.

```gos
let point = [3, 4]
let zeros = [0; 4]
let zero_vec: Vec<i64> = Vec::from([0; 4])
```

An expected `[T; N]` type can also shape a Vec literal:

```gos
let point: [i64; 2] = #[3, 4]
```

## Repeating A Value

The repeat form `[value; count]` follows the same spelling rule as the list
form: brackets build a fixed array, `#[...]` builds a Vec.

```gos
let grid = [5; 5]        // [i64; 5] - five copies of 5
let mut buffer = #[6; 7] // Vec<i64> - seven copies of 6
buffer.push(8)           // the Vec form still grows
```

The count may be any integer expression; a Vec repeat accepts a runtime
length, while a fixed array needs a constant so its length is part of its
type.

```gos
let width = 3
let row = #[0; width]
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

## Set And BTreeSet

Use `#{...}` for a `Set<T>`.

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

## Queue

A `Queue<i64>` is FIFO-only: `push` appends to the back and `pop` removes from
the front. Use `Queue::new()` for an empty queue and `Queue::from([...])` to
seed one in front-to-back order. `peek`, `len`, `is_empty`, and `clear` are the
common observers and the reset operation.

```gos
use std::collections::Queue

let mut q: Queue<i64> = Queue::from([10, 20])
q.push(30)
println(q.len())
println(q.peek())
println(q.pop())
```

## Stack

A `Stack<i64>` is LIFO-only: `push` appends to the top and `pop` removes from
the top. Use `Stack::new()` for an empty stack and `Stack::from([...])` to seed
one in bottom-to-top order.

```gos
use std::collections::Stack

let mut s: Stack<i64> = Stack::from([10, 20])
s.push(30)
println(s.len())
println(s.peek())
println(s.pop())
```

## Deque

Use `Deque<i64>` when both ends matter. It has explicit front/back methods.

```gos
use std::collections::Deque

let mut d: Deque<i64> = Deque::from([10, 20])
d.push_front(5)
d.push_back(30)
println(d.pop_front())
println(d.pop_back())
```

## MaxHeap and MinHeap

`MaxHeap<i64>` pops the largest value and `MinHeap<i64>` the smallest, so
neither needs a negated key or a wrapper type.

```gos
use std::collections::{MaxHeap, MinHeap}

let mut max_heap = MaxHeap::from([5, 1, 3])
println(max_heap.peek())  // Some(5)
max_heap.push(8)
println(max_heap.pop())   // Some(8)

let mut min_heap = MinHeap::from([5, 1, 3])
println(min_heap.peek())  // Some(1)
min_heap.push(0)
println(min_heap.pop())   // Some(0)
```

## Tuples

A tuple groups a fixed number of values whose types may differ. It is written
with parentheses rather than brackets and needs no import.

```gos
let entry = (1, "two", 3.0)
println(entry.1)
let (id, name, weight) = entry
```

See [Tuples](language/tuples.md) for the full surface.

## Summary

| Literal | Result |
|---|---|
| `#[a, b]` | `Vec<T>` |
| `[a, b]` | `[T; N]` fixed array |
| `[value; count]` | repeated fixed array |
| `{}` | empty `Map<K, V>` |
| `{key: value}` | `Map<K, V>` |
| `#{a, b}` | `Set<T>`, or `BTreeSet<T>` with an expected type |
| `(a, b)` | tuple |

| Constructor | Result |
|---|---|
| `Queue::new()` / `Queue::from([a, b])` | `Queue<i64>` |
| `Stack::new()` / `Stack::from([a, b])` | `Stack<i64>` |
| `Deque::new()` / `Deque::from([a, b])` | `Deque<i64>` |
| `MaxHeap::new()` / `MaxHeap::from([a, b])` | `MaxHeap<i64>` |
| `MinHeap::new()` / `MinHeap::from([a, b])` | `MinHeap<i64>` |
