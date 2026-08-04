//! Runtime support for `std::collections`.
//! The Gossamer-facing collection types are named `Vec`, `HashMap`,
//! `BTreeMap`, ... to match the stdlib surface. Their concrete
//! implementations wrap the Rust standard library so
//! every operation is safe and battle-tested. Later phases may swap
//! the backing store for a GC-aware implementation without changing
//! the observable semantics.

#![forbid(unsafe_code)]

use std::collections::{
    BTreeMap, BTreeSet, BinaryHeap as StdBinaryHeap, HashMap, HashSet, VecDeque,
};

/// Dense sequence used by Gossamer's `Vec<T>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vector<T> {
    inner: Vec<T>,
}

impl<T> Vector<T> {
    /// Empty vector.
    #[must_use]
    pub fn new() -> Self {
        Self { inner: Vec::new() }
    }

    /// Vector with an initial capacity hint.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Vec::with_capacity(capacity),
        }
    }

    /// Appends `value` to the end.
    pub fn push(&mut self, value: T) {
        self.inner.push(value);
    }

    /// Removes and returns the last element.
    pub fn pop(&mut self) -> Option<T> {
        self.inner.pop()
    }

    /// Current element count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns `true` when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Borrows the element at `index`.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&T> {
        self.inner.get(index)
    }

    /// Iterates by reference.
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.inner.iter()
    }

    /// Returns the raw backing slice.
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.inner
    }
}

impl<T> Default for Vector<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> From<Vec<T>> for Vector<T> {
    fn from(inner: Vec<T>) -> Self {
        Self { inner }
    }
}

impl<'a, T> IntoIterator for &'a Vector<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.inner.iter()
    }
}

/// Double-ended queue wrapper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deque<T> {
    inner: VecDeque<T>,
}

/// Queue alias for Gossamer's `VecQueue<T>` spelling.
pub type VecQueue<T> = Deque<T>;

/// Max-heap wrapper.
#[derive(Debug, Clone)]
pub struct MaxHeap<T: Ord> {
    inner: StdBinaryHeap<T>,
}

/// Compatibility spelling for a max heap.
pub type BinaryHeap<T> = MaxHeap<T>;

impl<T: Ord> MaxHeap<T> {
    /// Empty max heap.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: StdBinaryHeap::new(),
        }
    }

    /// Pushes a value.
    pub fn push(&mut self, value: T) {
        self.inner.push(value);
    }

    /// Removes and returns the largest value.
    pub fn pop(&mut self) -> Option<T> {
        self.inner.pop()
    }

    /// Borrows the largest value.
    #[must_use]
    pub fn peek(&self) -> Option<&T> {
        self.inner.peek()
    }

    /// Current length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Empty check.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Removes all values.
    pub fn clear(&mut self) {
        self.inner.clear();
    }
}

impl<T: Ord> Default for MaxHeap<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Ord> From<Vec<T>> for MaxHeap<T> {
    fn from(inner: Vec<T>) -> Self {
        Self {
            inner: StdBinaryHeap::from(inner),
        }
    }
}

/// Min-heap wrapper.
#[derive(Debug, Clone)]
pub struct MinHeap<T: Ord> {
    inner: StdBinaryHeap<std::cmp::Reverse<T>>,
}

impl<T: Ord> MinHeap<T> {
    /// Empty min heap.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: StdBinaryHeap::new(),
        }
    }

    /// Pushes a value.
    pub fn push(&mut self, value: T) {
        self.inner.push(std::cmp::Reverse(value));
    }

    /// Removes and returns the smallest value.
    pub fn pop(&mut self) -> Option<T> {
        self.inner.pop().map(|value| value.0)
    }

    /// Borrows the smallest value.
    #[must_use]
    pub fn peek(&self) -> Option<&T> {
        self.inner.peek().map(|value| &value.0)
    }

    /// Current length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Empty check.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Removes all values.
    pub fn clear(&mut self) {
        self.inner.clear();
    }
}

impl<T: Ord> Default for MinHeap<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Ord> From<Vec<T>> for MinHeap<T> {
    fn from(inner: Vec<T>) -> Self {
        let mut heap = Self::new();
        for value in inner {
            heap.push(value);
        }
        heap
    }
}

impl<T> Deque<T> {
    /// Empty deque.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: VecDeque::new(),
        }
    }
    /// Pushes to the back.
    pub fn push_back(&mut self, value: T) {
        self.inner.push_back(value);
    }
    /// Pushes to the front.
    pub fn push_front(&mut self, value: T) {
        self.inner.push_front(value);
    }
    /// Pops from the front.
    pub fn pop_front(&mut self) -> Option<T> {
        self.inner.pop_front()
    }
    /// Pops from the back.
    pub fn pop_back(&mut self) -> Option<T> {
        self.inner.pop_back()
    }
    /// Current length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }
    /// Empty check.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl<T> Default for Deque<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Hash-map wrapper.
#[derive(Debug, Clone)]
pub struct HashMapS<K, V> {
    inner: HashMap<K, V>,
}

impl<K: std::hash::Hash + Eq, V> HashMapS<K, V> {
    /// Empty map.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }
    /// Inserts a key/value pair, returning the previous value if any.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.inner.insert(key, value)
    }
    /// Looks up a key.
    pub fn get(&self, key: &K) -> Option<&V> {
        self.inner.get(key)
    }
    /// Removes and returns the value for `key`.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        self.inner.remove(key)
    }
    /// Whether the map contains `key`.
    pub fn contains_key(&self, key: &K) -> bool {
        self.inner.contains_key(key)
    }
    /// Current length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }
    /// Empty check.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl<K: std::hash::Hash + Eq, V> Default for HashMapS<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

/// Ordered-map wrapper.
#[derive(Debug, Clone)]
pub struct TreeMap<K: Ord, V> {
    inner: BTreeMap<K, V>,
}

impl<K: Ord, V> TreeMap<K, V> {
    /// Empty map.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: BTreeMap::new(),
        }
    }
    /// Inserts a key/value pair.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.inner.insert(key, value)
    }
    /// Looks up a key.
    pub fn get(&self, key: &K) -> Option<&V> {
        self.inner.get(key)
    }
    /// Removes a key.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        self.inner.remove(key)
    }
    /// Current length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }
    /// Empty check.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl<K: Ord, V> Default for TreeMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

/// Hash-set wrapper.
#[derive(Debug, Clone)]
pub struct HashSetS<T> {
    inner: HashSet<T>,
}

impl<T: std::hash::Hash + Eq> HashSetS<T> {
    /// Empty set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: HashSet::new(),
        }
    }
    /// Inserts `value`, returning whether it was newly added.
    pub fn insert(&mut self, value: T) -> bool {
        self.inner.insert(value)
    }
    /// Membership test.
    pub fn contains(&self, value: &T) -> bool {
        self.inner.contains(value)
    }
    /// Current length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }
    /// Empty check.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl<T: std::hash::Hash + Eq> Default for HashSetS<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Ordered-set wrapper.
#[derive(Debug, Clone)]
pub struct TreeSet<T: Ord> {
    inner: BTreeSet<T>,
}

impl<T: Ord> TreeSet<T> {
    /// Empty set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: BTreeSet::new(),
        }
    }
    /// Inserts `value`.
    pub fn insert(&mut self, value: T) -> bool {
        self.inner.insert(value)
    }
    /// Membership test.
    pub fn contains(&self, value: &T) -> bool {
        self.inner.contains(value)
    }
    /// Current length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }
    /// Empty check.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl<T: Ord> Default for TreeSet<T> {
    fn default() -> Self {
        Self::new()
    }
}
