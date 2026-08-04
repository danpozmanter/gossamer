# Collection patterns

Use these shapes when you need common queue, deque, stack, or heap behavior.

## Queue

Use `VecDeque<i64>` when you need repeated FIFO operations. `VecQueue<i64>` is an alias for this queue shape, and `<[a, b, c]>` builds a queue literal in front-to-back order.

```gossamer
use std::collections::VecDeque

let mut q: VecDeque<i64> = VecDeque::new()
q.push_back(10)
q.push_back(20)
let first = q.pop_front()

let mut literal_q = <[10, 20]>
let literal_first = literal_q.pop_front()
```

For small `i64` examples, `std::collections::queue` also offers a re-bind helper API over `Vec<i64>`:

```gossamer
use std::collections::queue

let q = []
let q = queue::push(q, 10)
let q = queue::push(q, 20)
let first = queue::peek(&q)
let q = queue::pop(q)
```

## Dequeue

Use `VecDeque<i64>` when both ends matter. `VecDequeue<i64>` and `VecQueue<i64>` are accepted aliases, but docs and discovery use the canonical `VecDeque<i64>` name.

```gossamer
use std::collections::VecDeque

let mut d: VecDeque<i64> = VecDeque::new()
d.push_front(1)
d.push_back(2)
let front = d.pop_front()
let back = d.pop_back()

let mut literal_d = <[1, 2]>
literal_d.push_front(0)
```

For value-style `i64` code, the `std::collections::deque` module returns the updated vector from each mutator:

```gossamer
use std::collections::deque

let d = []
let d = deque::push_front(d, 1)
let d = deque::push_back(d, 2)
let front = deque::peek_front(&d)
```

## Stack

A stack is naturally a `Vec<T>`: push to the end and pop from the end.

```gossamer
let mut stack: Vec<i64> = []
stack.push(1)
stack.push(2)
let top = stack.pop()
```

The `std::collections::stack` module is the re-bind helper form over `Vec<i64>`:

```gossamer
use std::collections::stack

let s = []
let s = stack::push(s, 1)
let s = stack::push(s, 2)
let top = stack::peek(&s)
let s = stack::pop(s)
```

## Min Heap

Use `MinHeap<i64>` or the `_[...]` literal when the smallest value should come first.

```gossamer
let mut h = _[5, 1, 3]
let smallest = h.peek()
h.push(0)
let popped = h.pop()
```

## Max Heap

Use `MaxHeap<i64>` or the `^[...]` literal when the largest value should come first. `BinaryHeap<i64>` is accepted as a compatibility alias for `MaxHeap<i64>`.

```gossamer
let mut h = ^[5, 1, 3]
let largest = h.peek()
h.push(8)
let popped = h.pop()
```
