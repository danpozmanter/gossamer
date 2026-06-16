// `std::container::queue` / `std::container::stack` /
// `std::container::deque` - FIFO / LIFO / double-ended queue over
// `Vec<i64>`. All operations use the re-bind shape:
//
//   let q = queue::push(q, 1)
//   let q = queue::pop(q)
//
// Compiled tier mutates the underlying Vec in place and returns the
// same pointer; VM tier clones via `Arc::make_mut`.

#![forbid(unsafe_code)]
#![allow(
    missing_docs,
    reason = "trivial sequence-container ops mirror the canonical names"
)]

/// FIFO queue ops on `Vec<i64>`.
pub mod queue {
    /// Append `value` to the back of the queue.
    #[must_use]
    pub fn push(mut xs: Vec<i64>, value: i64) -> Vec<i64> {
        xs.push(value);
        xs
    }

    /// Remove and discard the front element.
    #[must_use]
    pub fn pop(mut xs: Vec<i64>) -> Vec<i64> {
        if !xs.is_empty() {
            xs.remove(0);
        }
        xs
    }

    /// Front element, or 0 if empty.
    #[must_use]
    pub fn peek(xs: &[i64]) -> i64 {
        xs.first().copied().unwrap_or(0)
    }

    /// Element count.
    #[must_use]
    pub fn len(xs: &[i64]) -> i64 {
        xs.len() as i64
    }
}

/// LIFO stack ops on `Vec<i64>`.
pub mod stack {
    /// Push `value` onto the top of the stack.
    #[must_use]
    pub fn push(mut xs: Vec<i64>, value: i64) -> Vec<i64> {
        xs.push(value);
        xs
    }

    /// Pop the top of the stack.
    #[must_use]
    pub fn pop(mut xs: Vec<i64>) -> Vec<i64> {
        xs.pop();
        xs
    }

    /// Top of the stack, or 0 if empty.
    #[must_use]
    pub fn peek(xs: &[i64]) -> i64 {
        xs.last().copied().unwrap_or(0)
    }

    /// Element count.
    #[must_use]
    pub fn len(xs: &[i64]) -> i64 {
        xs.len() as i64
    }
}

/// Double-ended queue ops on `Vec<i64>`.
pub mod deque {
    /// Append to the back.
    #[must_use]
    pub fn push_back(mut xs: Vec<i64>, value: i64) -> Vec<i64> {
        xs.push(value);
        xs
    }

    /// Prepend to the front.
    #[must_use]
    pub fn push_front(mut xs: Vec<i64>, value: i64) -> Vec<i64> {
        xs.insert(0, value);
        xs
    }

    /// Drop the back element.
    #[must_use]
    pub fn pop_back(mut xs: Vec<i64>) -> Vec<i64> {
        xs.pop();
        xs
    }

    /// Drop the front element.
    #[must_use]
    pub fn pop_front(mut xs: Vec<i64>) -> Vec<i64> {
        if !xs.is_empty() {
            xs.remove(0);
        }
        xs
    }

    /// Front element, or 0 if empty.
    #[must_use]
    pub fn peek_front(xs: &[i64]) -> i64 {
        xs.first().copied().unwrap_or(0)
    }

    /// Back element, or 0 if empty.
    #[must_use]
    pub fn peek_back(xs: &[i64]) -> i64 {
        xs.last().copied().unwrap_or(0)
    }

    /// Element count.
    #[must_use]
    pub fn len(xs: &[i64]) -> i64 {
        xs.len() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_fifo() {
        let q = vec![];
        let q = queue::push(q, 1);
        let q = queue::push(q, 2);
        let q = queue::push(q, 3);
        assert_eq!(queue::peek(&q), 1);
        let q = queue::pop(q);
        assert_eq!(queue::peek(&q), 2);
        assert_eq!(queue::len(&q), 2);
    }

    #[test]
    fn stack_lifo() {
        let s = vec![];
        let s = stack::push(s, 1);
        let s = stack::push(s, 2);
        let s = stack::push(s, 3);
        assert_eq!(stack::peek(&s), 3);
        let s = stack::pop(s);
        assert_eq!(stack::peek(&s), 2);
    }

    #[test]
    fn deque_both_ends() {
        let d = vec![];
        let d = deque::push_back(d, 2);
        let d = deque::push_back(d, 3);
        let d = deque::push_front(d, 1);
        assert_eq!(deque::peek_front(&d), 1);
        assert_eq!(deque::peek_back(&d), 3);
        let d = deque::pop_front(d);
        assert_eq!(deque::peek_front(&d), 2);
    }
}
