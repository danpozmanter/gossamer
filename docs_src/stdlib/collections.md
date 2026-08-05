# `std::collections`

Status: experimental

Built-in container types.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`BTreeMap`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `type BTreeMap` | Ordered key-value map backed by BTreeMap. |
| [`BTreeSet`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `type BTreeSet` | Ordered unique-value set backed by BTreeSet. |
| [`BinaryHeap`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `type BinaryHeap` | Alias for `MaxHeap<T>`; underlying storage is BinaryHeap. |
| [`MaxHeap`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `type MaxHeap` | Max-priority heap (MaxBinaryHeap) backed by BinaryHeap; use `^[1, 2, 3]` for literals. |
| [`MinHeap`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `type MinHeap` | Min-priority heap (MinBinaryHeap) backed by BinaryHeap; use `_[1, 2, 3]` for literals. |
| [`Map`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `type Map` | Key-value map (HashMap) backed by the swiss-table layout. |
| [`Set`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `type Set` | Unique-value set (HashSet) backed by a hash table. |
| [`Vec`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `type Vec` | Growable contiguous sequence. |
| [`Deque`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `type Deque` | Double-ended queue (VecDeque) with explicit front/back methods such as `push_back` and `pop_front`. |
| [`Queue`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `type Queue` | FIFO queue (VecQueue) backed by VecDeque; use `<[1, 2, 3]` for literals and `push`, `pop`, `peek`, `len`, `is_empty`, and `clear`. |
| [`Stack`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `type Stack` | LIFO stack (VecStack) backed by VecDeque; use `[1, 2, 3]>` for literals and `push`, `pop`, `peek`, `len`, `is_empty`, and `clear`. |
| [`len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn len(xs: Vec<i64>) -> i64` | Element count. |
| [`peek_back`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn peek_back(xs: Vec<i64>) -> Option<i64>` | Back element, if present. |
| [`peek_front`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn peek_front(xs: Vec<i64>) -> Option<i64>` | Front element, if present. |
| [`pop_back`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn pop_back(xs: Vec<i64>) -> Vec<i64>` | Drop the back. |
| [`pop_front`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn pop_front(xs: Vec<i64>) -> Vec<i64>` | Drop the front. |
| [`push_back`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn push_back(xs: Vec<i64>, value: i64) -> Vec<i64>` | Append to the back. |
| [`push_front`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn push_front(xs: Vec<i64>, value: i64) -> Vec<i64>` | Prepend to the front. |
| [`len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn len(xs: Vec<i64>) -> i64` | Element count. |
| [`peek`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn peek(xs: Vec<i64>) -> Option<i64>` | Smallest element of the heap, if present. |
| [`pop`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn pop(xs: Vec<i64>) -> Vec<i64>` | Drop the root from the heap; returns the new heap (use `peek` first to read the value). |
| [`push`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn push(xs: Vec<i64>, value: i64) -> Vec<i64>` | Push an i64 onto the min-heap; returns the new heap. |
| [`contains_key`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn contains_key(map: OrderedMap<String, i64>, key: String) -> bool` | Key-membership test. |
| [`get`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn get(map: OrderedMap<String, i64>, key: String) -> Option<i64>` | Lookup; returns 0 if absent. |
| [`insert`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn insert(map: OrderedMap<String, i64>, key: String, value: i64) -> OrderedMap<String, i64>` | Set key => value. |
| [`len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn len(map: OrderedMap<String, i64>) -> i64` | Pair count. |
| [`remove`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn remove(map: OrderedMap<String, i64>, key: String) -> OrderedMap<String, i64>` | Remove a key. |
| [`contains`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn contains(xs: OrderedSet<i64>, value: i64) -> bool` | Membership test. |
| [`insert`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn insert(xs: OrderedSet<i64>, value: i64) -> OrderedSet<i64>` | Insert (sorted, no duplicates). |
| [`len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn len(xs: OrderedSet<i64>) -> i64` | Element count. |
| [`remove`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn remove(xs: OrderedSet<i64>, value: i64) -> OrderedSet<i64>` | Remove a value. |
| [`contains`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn contains(xs: Vec<i64>, value: i64) -> bool` | Return true iff `value` is present. |
| [`index_of`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn index_of(xs: Vec<i64>, value: i64) -> Option<i64>` | Return the index of `value`, or -1. |
| [`insert`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn insert(xs: Vec<i64>, value: i64) -> Vec<i64>` | Insert at the unique sorted position. |
| [`len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn len(xs: Vec<i64>) -> i64` | Element count. |
| [`peek_max`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn peek_max(xs: Vec<i64>) -> i64` | Largest element, or 0. |
| [`peek_min`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn peek_min(xs: Vec<i64>) -> i64` | Smallest element, or 0. |
| [`remove_at`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn remove_at(xs: Vec<i64>, index: i64) -> Vec<i64>` | Remove the element at index `i`. |
| [`len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn len(xs: Vec<i64>) -> i64` | Element count. |
| [`peek`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn peek(xs: Vec<i64>) -> Option<i64>` | Front element, if present. |
| [`pop`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn pop(xs: Vec<i64>) -> Vec<i64>` | Drop the front element; returns the new queue. |
| [`push`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn push(xs: Vec<i64>, value: i64) -> Vec<i64>` | Append an i64 to the back; returns the new queue. |
| [`len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn len(xs: Vec<i64>) -> i64` | Element count. |
| [`peek`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn peek(xs: Vec<i64>) -> Option<i64>` | Top element, if present. |
| [`pop`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn pop(xs: Vec<i64>) -> Vec<i64>` | Drop the top; returns the new stack. |
| [`push`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn push(xs: Vec<i64>, value: i64) -> Vec<i64>` | Push an i64 onto the top; returns the new stack. |

## `Set<T>` methods

`Set` provides `new`, `insert`, `remove`, `contains`, `len`, `is_empty`,
`clear`, `iter`, `to_vec`, `union`, `intersection`, `difference`,
`symmetric_difference`, `is_subset`, `is_superset`, and `is_disjoint`.
Use `#{a, b, c}` for a `Set` literal.

As in Rust, `map` is an iterator method rather than a `Set` method. Use
`set.iter().map(f)`. Calling `set.map(f)` is a type error.

## `BTreeSet<T>` methods

`BTreeSet` provides the same set method surface as `Set`, but iteration
and `to_vec` return values in sorted order. Use an expected type to shape a
set literal:

```gos
let ordered: BTreeSet<i64> = #{3, 1, 2, 1}
println(ordered.to_vec())
```
