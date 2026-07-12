//! Session-scoped string interning backing [`Symbol`] handles.
//!
//! A compiler daemon must not retain every identifier it has ever seen.
//! Earlier versions used a process-global interner and leaked each spelling
//! in order to make `Symbol::as_str` return `&'static str`.  That made the
//! resident set grow monotonically for the lifetime of the REPL, LSP, and
//! long-running build processes.
//!
//! [`SymbolInterner`] is deliberately owned by its caller.  Symbols keep an
//! `Arc` to their spelling, so they remain valid when the interner is dropped;
//! the interner only owns the deduplication index.  Once both the session and
//! its symbols are dropped, all associated identifier storage is reclaimed.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

/// Interned identifier handle.
///
/// Equality, ordering, and hashing use the spelling rather than a
/// session-local numeric index.  Symbols may therefore safely cross session
/// boundaries (for example, in a diagnostic retained after a frontend pass).
#[derive(Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Symbol(Arc<str>);

impl Symbol {
    /// Creates an owned symbol without retaining it in a process-global
    /// interner.
    ///
    /// Prefer [`SymbolInterner::intern`] for repeated names in one compiler
    /// session.  This compatibility constructor intentionally does not share
    /// state: callers that do not own an interner must not silently extend a
    /// process-lifetime allocation arena.
    #[must_use]
    pub fn intern(s: &str) -> Self {
        Self(Arc::from(s))
    }

    /// Returns the original spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Symbol({:?})", self.as_str())
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

/// Compatibility hook for callers that previously cleared the global
/// interner between independent inputs.
///
/// There is no process-global interner to reset anymore.  Storage is released
/// by dropping each [`SymbolInterner`] and every outstanding [`Symbol`].
pub fn reset_interner() {}

/// String interner owned by one compiler, parser, or tooling session.
///
/// The index holds weak references so [`Self::prune_unused`] can release
/// spellings no longer referenced by symbols before the whole session ends.
/// This is useful to a REPL or LSP that keeps its session object while
/// repeatedly replacing documents.
#[derive(Default)]
pub struct SymbolInterner {
    entries: HashMap<Box<str>, Weak<str>>,
}

impl SymbolInterner {
    /// Creates an empty session-local interner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks up or installs `spelling` in this session.
    #[must_use]
    pub fn intern(&mut self, spelling: &str) -> Symbol {
        if let Some(symbol) = self.entries.get(spelling).and_then(Weak::upgrade) {
            return Symbol(symbol);
        }

        let symbol: Arc<str> = Arc::from(spelling);
        self.entries
            .insert(Box::from(spelling), Arc::downgrade(&symbol));
        Symbol(symbol)
    }

    /// Drops stale index entries for symbols no longer held by the caller.
    /// Returns the number of entries removed.
    pub fn prune_unused(&mut self) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|_, spelling| spelling.strong_count() != 0);
        before - self.entries.len()
    }

    /// Number of spellings currently tracked by this session.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` when no spellings are tracked by this session.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{Symbol, SymbolInterner};

    #[test]
    fn session_interning_deduplicates_a_spelling() {
        let mut interner = SymbolInterner::new();
        let a = interner.intern("foo");
        let b = interner.intern("foo");
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "foo");
        assert_eq!(interner.len(), 1);
    }

    #[test]
    fn symbols_outlive_the_session_index() {
        let symbol = {
            let mut interner = SymbolInterner::new();
            interner.intern("survives")
        };
        assert_eq!(symbol.as_str(), "survives");
    }

    #[test]
    fn pruning_reclaims_unreferenced_spellings() {
        let mut interner = SymbolInterner::new();
        let symbol = interner.intern("temporary");
        drop(symbol);
        assert_eq!(interner.prune_unused(), 1);
        assert!(interner.is_empty());
    }

    #[test]
    fn compatibility_constructor_is_not_global() {
        let a = Symbol::intern("alpha");
        let b = Symbol::intern("alpha");
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "alpha");
    }

    #[test]
    fn symbols_from_independent_sessions_compare_by_spelling() {
        let mut first = SymbolInterner::new();
        let mut second = SymbolInterner::new();
        assert_eq!(first.intern("shared"), second.intern("shared"));
    }
}
