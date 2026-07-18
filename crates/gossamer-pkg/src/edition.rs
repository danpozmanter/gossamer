//! Target-independent source-language edition selection.

/// Source-language edition accepted by this toolchain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edition {
    /// Current edition with eager public iterator helpers.
    E2026,
    /// Edition with linear lazy public iterator helpers.
    E2027,
}

impl Edition {
    /// Canonical manifest spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::E2026 => "2026",
            Self::E2027 => "2027",
        }
    }
}
