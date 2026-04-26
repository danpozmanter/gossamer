//! Lexical environment used by the tree-walking interpreter.

#![forbid(unsafe_code)]

use crate::value::Value;

/// Stack of name-to-value frames corresponding to lexical scopes.
///
/// Frames use a `Vec<(String, Value)>` rather than a hash map: the
/// shape is dominated by short fn bodies with single-digit locals,
/// where linear scan + cache locality beats `HashMap` (no per-lookup
/// hash compute, no bucket pointer chase). Insertion is also faster
/// because a `Vec::push` doesn't grow buckets. For the
/// pathological case of >32 locals in one frame the asymptotic
/// crossover is still worth taking — programs that wide are rare
/// and the constant on `HashMap<String>` was the real cost.
#[derive(Debug, Default, Clone)]
pub struct Env {
    frames: Vec<Vec<(String, Value)>>,
}

impl Env {
    /// Returns a fresh environment with a single empty top frame.
    #[must_use]
    pub fn new() -> Self {
        Self {
            frames: vec![Vec::with_capacity(8)],
        }
    }

    /// Pushes a new empty frame onto the stack. Call on block entry,
    /// function entry, or closure application.
    pub fn push(&mut self) {
        // Pre-allocate to the typical fn-body local count so the
        // first few `bind` calls don't grow the underlying buffer.
        self.frames.push(Vec::with_capacity(8));
    }

    /// Pops the top frame. Callers must balance this with `push`.
    pub fn pop(&mut self) {
        self.frames.pop();
    }

    /// Declares `name` in the top frame, shadowing any outer binding.
    pub fn bind(&mut self, name: impl Into<String>, value: Value) {
        let name = name.into();
        if let Some(frame) = self.frames.last_mut() {
            // Shadowing semantics: a fresh `let` in the same frame
            // updates the existing slot if present, otherwise appends.
            // Linear scan is fine — a single frame rarely exceeds
            // ~16 locals and scanning is cache-friendly.
            for (existing, slot) in frame.iter_mut() {
                if existing == &name {
                    *slot = value;
                    return;
                }
            }
            frame.push((name, value));
        }
    }

    /// Assigns `value` to an existing binding named `name`, searching
    /// from innermost to outermost frame. Returns `true` on success.
    pub fn assign(&mut self, name: &str, value: Value) -> bool {
        for frame in self.frames.iter_mut().rev() {
            for (existing, slot) in frame.iter_mut() {
                if existing == name {
                    *slot = value;
                    return true;
                }
            }
        }
        false
    }

    /// Looks up `name` from innermost to outermost frame.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<&Value> {
        for frame in self.frames.iter().rev() {
            for (existing, value) in frame {
                if existing == name {
                    return Some(value);
                }
            }
        }
        None
    }

    /// Flattens every live binding into a captured name/value list,
    /// preserving inner-frame precedence. Used when constructing
    /// closures so the captured environment is disjoint from the
    /// calling stack.
    #[must_use]
    pub fn capture_all(&self) -> Vec<(String, Value)> {
        let mut seen: Vec<(String, Value)> = Vec::new();
        for frame in self.frames.iter().rev() {
            for (name, value) in frame {
                if !seen.iter().any(|(n, _)| n == name) {
                    seen.push((name.clone(), value.clone()));
                }
            }
        }
        seen
    }
}
