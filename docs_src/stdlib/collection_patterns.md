# Collection patterns

Use these shapes when you need common queue, deque, stack, or heap behavior.

## Queue

Use `Queue<i64>` when you need repeated FIFO operations. `Queue::from([a, b, c])`
seeds one in front-to-back order.

```gossamer
use std::collections::Queue

let mut q: Queue<i64> = Queue::new()
q.push(10)
q.push(20)
let queued = q.len()
let next = q.peek()
let first = q.pop()

let mut seeded = Queue::from(#[10, 20])
let seeded_first = seeded.pop()
```

For small `i64` examples, `std::collections::queue` also offers a re-bind helper API over `Vec<i64>`:

```gossamer
use std::collections::queue

let q = #[]
let q = queue::push(q, 10)
let q = queue::push(q, 20)
let first = queue::peek(&q)
let q = queue::pop(q)
```

## Deque

Use `Deque<i64>` when both ends matter.

```gossamer
use std::collections::Deque

let mut d: Deque<i64> = Deque::new()
d.push_front(1)
d.push_back(2)
let front = d.pop_front()
let back = d.pop_back()

let mut from_values = Deque::from(#[1, 2])
from_values.push_front(0)
```

For value-style `i64` code, the `std::collections::deque` module returns the updated vector from each mutator:

```gossamer
use std::collections::deque

let d = #[]
let d = deque::push_front(d, 1)
let d = deque::push_back(d, 2)
let front = deque::peek_front(&d)
```

## Stack

Use `Stack<i64>` when you need repeated LIFO operations. `Stack::from([a, b, c])`
seeds one in bottom-to-top order.

```gossamer
use std::collections::Stack

let mut stack: Stack<i64> = Stack::from(#[1])
stack.push(1)
stack.push(2)
let depth = stack.len()
let next = stack.peek()
let top = stack.pop()
```

The `std::collections::stack` module is the re-bind helper form over `Vec<i64>`:

```gossamer
use std::collections::stack

let s = #[]
let s = stack::push(s, 1)
let s = stack::push(s, 2)
let top = stack::peek(&s)
let s = stack::pop(s)
```

## Min Heap

Use `MinHeap<i64>` when the smallest value should come first, rather than
negating keys into a max heap.

```gossamer
use std::collections::MinHeap

let mut h = MinHeap::from(#[5, 1, 3])
let smallest = h.peek()
h.push(0)
let popped = h.pop()
```

## Max Heap

Use `MaxHeap<i64>` when the largest value should come first.

```gossamer
use std::collections::MaxHeap

let mut h = MaxHeap::from(#[5, 1, 3])
let largest = h.peek()
h.push(8)
let popped = h.pop()
```
