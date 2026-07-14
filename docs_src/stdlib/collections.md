# `std::collections`

Status: experimental

Built-in container types.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Vec` | type | Growable contiguous sequence. |
| `VecDeque` | type | Double-ended queue backed by a ring buffer. |
| `HashMap` | type | Hash map backed by the swiss-table layout. |
| `BTreeMap` | type | Ordered map. |
| `HashSet` | type | Unordered set built on top of `HashMap`. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`BTreeMap`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `type BTreeMap` | Ordered map. |
| [`HashMap`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `type HashMap` | Hash map backed by the swiss-table layout. |
| [`HashSet`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `type HashSet` | Unordered set built on top of `HashMap`. |
| [`Vec`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `type Vec` | Growable contiguous sequence. |
| [`VecDeque`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `type VecDeque` | Double-ended queue backed by a ring buffer. |
| [`len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn len(xs: Deque<i64>) -> i64` | Element count. |
| [`peek_back`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn peek_back(xs: Deque<i64>) -> i64` | Back element, or 0 if empty. |
| [`peek_front`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn peek_front(xs: Deque<i64>) -> i64` | Front element, or 0 if empty. |
| [`pop_back`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn pop_back(xs: Deque<i64>) -> Deque<i64>` | Drop the back. |
| [`pop_front`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn pop_front(xs: Deque<i64>) -> Deque<i64>` | Drop the front. |
| [`push_back`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn push_back(xs: Deque<i64>, value: i64) -> Deque<i64>` | Append to the back. |
| [`push_front`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn push_front(xs: Deque<i64>, value: i64) -> Deque<i64>` | Prepend to the front. |
| [`len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn len(xs: Heap<i64>) -> i64` | Element count. |
| [`peek`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn peek(xs: Heap<i64>) -> i64` | Smallest element of the heap, or 0 if empty. |
| [`pop`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn pop(xs: Heap<i64>) -> Heap<i64>` | Drop the root from the heap; returns the new heap (use `peek` first to read the value). |
| [`push`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn push(xs: Heap<i64>, value: i64) -> Heap<i64>` | Push an i64 onto the min-heap; returns the new heap. |
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
| [`len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn len(xs: Queue<i64>) -> i64` | Element count. |
| [`peek`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn peek(xs: Queue<i64>) -> i64` | Front element, or 0 if empty. |
| [`pop`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn pop(xs: Queue<i64>) -> Queue<i64>` | Drop the front element; returns the new queue. |
| [`push`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn push(xs: Queue<i64>, value: i64) -> Queue<i64>` | Append an i64 to the back; returns the new queue. |
| [`len`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn len(xs: Stack<i64>) -> i64` | Element count. |
| [`peek`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn peek(xs: Stack<i64>) -> i64` | Top element, or 0 if empty. |
| [`pop`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn pop(xs: Stack<i64>) -> Stack<i64>` | Drop the top; returns the new stack. |
| [`push`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/collections.rs) | `fn push(xs: Stack<i64>, value: i64) -> Stack<i64>` | Push an i64 onto the top; returns the new stack. |
