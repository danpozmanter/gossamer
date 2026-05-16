// `std::container::heap` — binary min-heap over `Vec<i64>`.
//
// The shape avoids in-place mutation through a `&mut` reference
// (which doesn't propagate cleanly across the VM tier's Arc-backed
// Value::Array). Instead each operation returns the heap so the
// caller re-binds:
//
//   let h = []
//   let h = heap::push(h, 5)
//   let h = heap::push(h, 1)
//   while heap::len(&h) > 0 {
//       let v = heap::peek(&h)
//       process(v)
//       let h = heap::pop(h)
//   }
//
// Compiled tier mutates in place and returns the same heap pointer;
// VM tier clones via `Arc::make_mut` and returns the new Vec —
// both reach the user as the right Vec<i64> after the re-bind.

#![forbid(unsafe_code)]

/// Push `value` onto the min-heap `xs`, returning the new heap.
#[must_use]
pub fn push(xs: Vec<i64>, value: i64) -> Vec<i64> {
    let mut xs = xs;
    xs.push(value);
    let mut i = xs.len() - 1;
    while i > 0 {
        let parent = (i - 1) / 2;
        if xs[parent] > xs[i] {
            xs.swap(parent, i);
            i = parent;
        } else {
            break;
        }
    }
    xs
}

/// Drop the root of the heap (caller should `peek` first to read
/// the popped value). Returns the new heap.
#[must_use]
pub fn pop(xs: Vec<i64>) -> Vec<i64> {
    let mut xs = xs;
    if xs.is_empty() {
        return xs;
    }
    let last = xs.len() - 1;
    xs.swap(0, last);
    xs.pop();
    let n = xs.len();
    if n > 1 {
        let mut i = 0;
        loop {
            let l = 2 * i + 1;
            let r = 2 * i + 2;
            let mut smallest = i;
            if l < n && xs[l] < xs[smallest] {
                smallest = l;
            }
            if r < n && xs[r] < xs[smallest] {
                smallest = r;
            }
            if smallest == i {
                break;
            }
            xs.swap(smallest, i);
            i = smallest;
        }
    }
    xs
}

/// Root element (smallest) of the heap, or 0 if empty. Caller is
/// expected to check `len` first.
#[must_use]
pub fn peek(xs: &[i64]) -> i64 {
    xs.first().copied().unwrap_or(0)
}

/// Number of elements in the heap.
#[must_use]
pub fn len(xs: &[i64]) -> i64 {
    xs.len() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_pop_orders_min_first() {
        let h = vec![];
        let h = push(h, 5);
        let h = push(h, 1);
        let h = push(h, 3);
        let h = push(h, 2);
        assert_eq!(peek(&h), 1);
        let h = pop(h);
        assert_eq!(peek(&h), 2);
        let h = pop(h);
        assert_eq!(peek(&h), 3);
        let h = pop(h);
        assert_eq!(peek(&h), 5);
        let h = pop(h);
        assert_eq!(len(&h), 0);
    }

    #[test]
    fn peek_on_empty_returns_zero() {
        let h: Vec<i64> = vec![];
        assert_eq!(peek(&h), 0);
    }
}
