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
    /// `i128` — the 2-word by-value representation of `Result`/`Option`
    /// (discriminant in the low 64 bits, payload in the high 64 bits).
    I128,
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
    /// When true the function may unwind, so the declaration omits the
    /// `nounwind` attribute (even when `noreturn` is set). Required for
    /// `gos_rt_panic`, which raises a Rust panic on the goroutine path
    /// that must propagate across its caller to the coroutine catch.
    pub unwinds: bool,
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
            AbiType::I128 => "i128",
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
        self.llvm_declare_for(cfg!(windows))
    }

    /// `llvm_declare` parameterised on whether the target is Win64, so the
    /// platform-specific `i128` marshalling is unit-testable on any host.
    ///
    /// `Win64` marshals the 2-word `i128` (Fat) representation across the
    /// `extern "C"` boundary differently from a GP register pair: an `i128`
    /// *argument* is passed by pointer, and an `i128` *return* comes back in
    /// a 16-byte vector register (`<16 x i8>`). This matches how rustc
    /// lowers `i128` in an `extern "C"` signature on `x86_64-pc-windows`;
    /// emitting a bare `i128` makes llc pick the GP-pair ABI, which the
    /// Rust runtime does not use, corrupting every Result/Option crossing
    /// the boundary. The matching call-site marshalling lives in
    /// `lower_runtime_call_intrinsic` / `emit_named_call`. On `SysV`
    /// (Linux/macOS) bare `i128` already agrees between llc and rustc.
    #[must_use]
    pub fn llvm_declare_for(&self, win: bool) -> String {
        let param_ir = |t: &AbiType| -> &'static str {
            if win && *t == AbiType::I128 {
                "ptr"
            } else {
                t.llvm_ir()
            }
        };
        let ret_ir = if win && self.sig.ret == AbiType::I128 {
            "<16 x i8>"
        } else {
            self.sig.ret.llvm_ir()
        };
        let params = self
            .sig
            .params
            .iter()
            .map(param_ir)
            .collect::<Vec<_>>()
            .join(", ");
        if self.noreturn {
            // `noreturn` functions never return normally, but one that
            // may unwind (`gos_rt_panic` on the goroutine path) must NOT
            // be `nounwind` — that would abort the unwind at any cleanup
            // frame. LLVM permits `noreturn` together with a may-unwind
            // function.
            let tail = if self.unwinds {
                "noreturn cold"
            } else {
                "noreturn cold nounwind"
            };
            format!("declare {} @{}({}) {tail}", ret_ir, self.name, params)
        } else {
            // Every `gos_rt_*` symbol is an `extern "C"` Rust function, and
            // unwinding out of an `extern "C"` boundary aborts (Rust never
            // propagates a panic across it) — so the call cannot unwind. The
            // `nounwind` attribute makes that explicit to LLVM, which would
            // otherwise treat every runtime call as a potential exception
            // edge: that blocks reordering, hoisting (LICM), and CSE of the
            // surrounding loads/stores in every hot loop that calls a runtime
            // helper. (`willreturn`/`memory` are intentionally not blanket-
            // applied: a helper may abort on a Rust panic — not a return — and
            // most touch global allocator state.)
            //
            // A small audited allowlist of pure getters additionally gets
            // `memory(argmem: read)`: they only *read* memory reachable
            // through their pointer arguments (no writes, no global state).
            // This is what lets `opt` hoist a loop-invariant `graph[node]` /
            // `visited[nb]` read out of a loop and CSE repeated reads —
            // `nounwind` alone is insufficient because, without a memory-effect
            // bound, LLVM must assume the call clobbers all memory.
            let attrs = if PURE_ARGMEM_READ.contains(&self.name) {
                "nounwind memory(argmem: read)"
            } else {
                "nounwind"
            };
            format!("declare {} @{}({}) {attrs}", ret_ir, self.name, params)
        }
    }
}

/// Runtime getters that only read memory reachable through their pointer
/// arguments — no writes, no global state. Marked `memory(argmem: read)` so
/// the optimiser can hoist/CSE them across non-aliasing loop bodies. Keep
/// this list conservative: a wrong entry (a helper that writes or reads
/// globals) is a miscompile.
const PURE_ARGMEM_READ: &[&str] = &[
    "gos_rt_vec_get_i64",
    "gos_rt_vec_get_i64_unchecked",
    "gos_rt_vec_get_i128",
    "gos_rt_vec_get_ptr",
    "gos_rt_vec_len",
    "gos_rt_arr_len",
    "gos_rt_str_len",
    "gos_rt_str_byte_at",
    "gos_rt_str_eq",
    "gos_rt_heap_i64_get",
];
