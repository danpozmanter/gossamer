//! Identifier types used throughout the HIR.

#![forbid(unsafe_code)]

/// Monotonic identifier assigned to every HIR node. Independent from the
/// AST's `NodeId` so that lowering can introduce synthetic nodes for
/// desugared constructs without clashing with parser-issued ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirId(pub u32);

impl HirId {
    /// Raw numeric index of this identifier.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Hands out fresh [`HirId`]s in monotonic order.
#[derive(Debug, Default, Clone)]
pub struct HirIdGenerator {
    next: u32,
}

impl HirIdGenerator {
    /// Returns a fresh generator rooted at `0`.
    #[must_use]
    pub const fn new() -> Self {
        Self { next: 0 }
    }

    /// Produces the next fresh identifier.
    #[allow(
        clippy::should_implement_trait,
        reason = "infallible generator; Iterator::next would force Option<HirId>"
    )]
    pub fn next(&mut self) -> HirId {
        let id = HirId(self.next);
        self.next = self.next.saturating_add(1);
        id
    }

    /// Returns the number of identifiers handed out so far.
    #[must_use]
    pub const fn issued(&self) -> u32 {
        self.next
    }
}
