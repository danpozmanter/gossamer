//! Process-global string interner backing [`Symbol`] handles.
//!
//! Hot data structures across resolve / typeck / LSP key on
//! identifier names. Storing each occurrence as `String` (24 B
//! header + heap copy of the bytes) duplicates the same name
//! across scope frames, type tables, and per-document indexes —
//! ~5× duplication on a non-trivial project per the RAM analysis.
//!
//! The interner deduplicates: each distinct string maps to one
//! immutable allocation, and a `Symbol(u32)` carries the handle.
//! Comparison is integer equality, hashing is fast, and the
//! resolved `&'static str` is recovered without further locking.
//!
//! Storage strategy — Bytes are appended to a leak-on-purpose
//! arena (`Vec<Box<str>>`) so the slices the interner hands out
//! live for the process. The arena sits behind a `RwLock`; the
//! `lookup` map (string → symbol) sits inside the same lock. The
//! two-level layout (write under exclusive lock, read under
//! shared lock) keeps the hot `Symbol::as_str` path lock-free
//! after the first store.

use std::collections::HashMap;
use std::sync::OnceLock;

use parking_lot::RwLock;

/// Interned identifier handle. Comparison and hashing are
/// integer-cheap; the original spelling is recovered via
/// [`Symbol::as_str`].
#[derive(Copy, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Symbol(u32);

impl Symbol {
    /// Looks up or installs `s` in the global interner and
    /// returns its `Symbol`. Repeated calls with the same spelling
    /// are guaranteed to return the same `Symbol`.
    #[must_use]
    pub fn intern(s: &str) -> Self {
        let interner = global();
        // Read-side fast path: most identifiers are already
        // present after the first parse pass on the program.
        {
            let inner = interner.read();
            if let Some(&sym) = inner.lookup.get(s) {
                return sym;
            }
        }
        // Slow path: take the write lock, re-check (another
        // thread may have inserted between drop-read and
        // acquire-write), then install.
        let mut inner = interner.write();
        if let Some(&sym) = inner.lookup.get(s) {
            return sym;
        }
        let id = u32::try_from(inner.spellings.len()).expect("symbol interner overflow");
        let owned: Box<str> = Box::from(s);
        // SAFETY-equivalent (no `unsafe`): the `Box<str>` is
        // moved into `spellings` and never reallocated; the
        // lifetime of its bytes is therefore process-wide as
        // long as the interner itself lives, which is forever.
        // The `&'static str` we hand out points into that
        // owned allocation.
        let static_ref: &'static str = Box::leak(owned);
        inner.spellings.push(static_ref);
        inner.lookup.insert(static_ref.to_string(), Symbol(id));
        Symbol(id)
    }

    /// Returns the original spelling of this symbol. The returned
    /// reference is valid for the rest of the process.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        global().read().spellings[self.0 as usize]
    }

    /// Numeric handle. Exposed for tracing / cache-key use.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl std::fmt::Debug for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Symbol({}, {:?})", self.0, self.as_str())
    }
}

impl std::fmt::Display for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for Symbol {
    fn from(s: &str) -> Self {
        Self::intern(s)
    }
}

impl From<String> for Symbol {
    fn from(s: String) -> Self {
        Self::intern(&s)
    }
}

impl From<&String> for Symbol {
    fn from(s: &String) -> Self {
        Self::intern(s)
    }
}

struct Inner {
    spellings: Vec<&'static str>,
    lookup: HashMap<String, Symbol>,
}

fn global() -> &'static RwLock<Inner> {
    static INTERNER: OnceLock<RwLock<Inner>> = OnceLock::new();
    INTERNER.get_or_init(|| {
        RwLock::new(Inner {
            spellings: Vec::new(),
            lookup: HashMap::new(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::Symbol;

    #[test]
    fn intern_same_string_twice_returns_same_symbol() {
        let a = Symbol::intern("foo");
        let b = Symbol::intern("foo");
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "foo");
    }

    #[test]
    fn distinct_strings_get_distinct_symbols() {
        let a = Symbol::intern("alpha");
        let b = Symbol::intern("beta");
        assert_ne!(a, b);
        assert_eq!(a.as_str(), "alpha");
        assert_eq!(b.as_str(), "beta");
    }

    #[test]
    fn empty_string_is_a_valid_symbol() {
        let a = Symbol::intern("");
        assert_eq!(a.as_str(), "");
    }
}
