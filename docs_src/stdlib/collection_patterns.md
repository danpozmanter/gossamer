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

let mut seeded = Queue::from([10, 20])
let seeded_first = seeded.pop()
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

let mut from_values = Deque::from([1, 2])
from_values.push_front(0)
```

## Stack

Use `Stack<i64>` when you need repeated LIFO operations. `Stack::from([a, b, c])`
seeds one in bottom-to-top order.

```gossamer
use std::collections::Stack

let mut stack: Stack<i64> = Stack::from([1])
stack.push(1)
stack.push(2)
let depth = stack.len()
let next = stack.peek()
let top = stack.pop()
```

## Min Heap

Use `MinHeap<i64>` when the smallest value should come first, rather than
negating keys into a max heap.

```gossamer
use std::collections::MinHeap

let mut h = MinHeap::from([5, 1, 3])
let smallest = h.peek()
h.push(0)
let popped = h.pop()
```

## Max Heap

Use `MaxHeap<i64>` when the largest value should come first.

```gossamer
use std::collections::MaxHeap

let mut h = MaxHeap::from([5, 1, 3])
let largest = h.peek()
h.push(8)
let popped = h.pop()
```
