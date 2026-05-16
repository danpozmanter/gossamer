/// Type of a single parameter or return value in the C-ABI.
///
/// Matches the LLVM IR types used in `declare` statements. `U64` is
/// `i64` at the IR level but carries unsigned semantics at the Rust
/// level. `Bool` maps to `i8` in C (zero = false, non-zero = true).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AbiType {
    /// `void` — only valid as a return type.
    Void,
    /// `i8` — used for bool returns and narrow C types.
    I8,
    /// `i32` — used for C `int` and discriminants.
    I32,
    /// `i64` — the default integer width.
    I64,
    /// `i64` at the IR level; unsigned contract at the Rust level.
    U64,
    /// `double` — 64-bit IEEE 754 float.
    F64,
    /// Opaque pointer (`ptr` in LLVM opaque-pointer mode).
    Ptr,
}

/// Signature of a runtime symbol: parameter list + return type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiSig {
    /// Ordered parameter types.
    pub params: &'static [AbiType],
    /// Return type. Use `AbiType::Void` for functions that return nothing.
    pub ret: AbiType,
}

/// Which compiled backend tier(s) emit calls to a runtime symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Called by both the LLVM and Cranelift backends.
    Both,
    /// Called only by the Cranelift backend.
    Cranelift,
    /// Called only by the LLVM backend.
    Llvm,
}

/// One entry in the ABI registry describing a `gos_rt_*` symbol.
#[derive(Debug, Clone)]
pub struct RuntimeEntry {
    /// Symbol name without the `@` sigil, e.g. `"gos_rt_vec_push"`.
    pub name: &'static str,
    /// Full C-ABI signature.
    pub sig: AbiSig,
    /// Which compiled backend tier(s) use this symbol.
    pub tier: Tier,
    /// One-line description for `gos explain` output.
    pub docs: &'static str,
    /// When true the LLVM declaration gains `noreturn cold nounwind` attributes.
    /// Only set for functions that provably never return (abort / panic paths).
    pub noreturn: bool,
}

impl AbiType {
    /// LLVM IR type name for this ABI type.
    #[must_use]
    pub fn llvm_ir(self) -> &'static str {
        match self {
            AbiType::Void => "void",
            AbiType::I8 => "i8",
            AbiType::I32 => "i32",
            AbiType::I64 | AbiType::U64 => "i64",
            AbiType::F64 => "double",
            AbiType::Ptr => "ptr",
        }
    }
}

impl RuntimeEntry {
    /// Produces the full `declare <ret> @<name>(<params>)` LLVM IR string.
    ///
    /// When `noreturn` is set the declaration gains `noreturn cold nounwind`
    /// attributes. LLVM uses these to classify the call site as a trap exit
    /// rather than a live successor, which lets the loop vectoriser treat
    /// loops with a guarded bounds check as effectively single-exit.
    #[must_use]
    pub fn llvm_declare(&self) -> String {
        let params = self
            .sig
            .params
            .iter()
            .map(|t| t.llvm_ir())
            .collect::<Vec<_>>()
            .join(", ");
        if self.noreturn {
            format!(
                "declare {} @{}({}) noreturn cold nounwind",
                self.sig.ret.llvm_ir(),
                self.name,
                params
            )
        } else {
            format!(
                "declare {} @{}({})",
                self.sig.ret.llvm_ir(),
                self.name,
                params
            )
        }
    }
}
