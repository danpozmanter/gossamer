//! Diagnostics emitted by the type checker.

#![forbid(unsafe_code)]

use std::fmt;

use gossamer_lex::Span;
use thiserror::Error;

/// One type-checker diagnostic paired with its source location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDiagnostic {
    /// Specific error variant.
    pub error: TypeError,
    /// Where in the source the error was detected.
    pub span: Span,
}

impl TypeDiagnostic {
    /// Constructs a diagnostic from its error and span.
    #[must_use]
    pub const fn new(error: TypeError, span: Span) -> Self {
        Self { error, span }
    }
}

impl fmt::Display for TypeDiagnostic {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "{}", self.error)
    }
}

/// Every failure mode the type checker can report.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TypeError {
    /// Two concrete types that should be equal are not.
    #[error("type mismatch: expected `{expected}`, found `{found}`")]
    TypeMismatch {
        /// Expected type, rendered via [`crate::render_ty`].
        expected: String,
        /// Found type, rendered via [`crate::render_ty`].
        found: String,
    },
    /// A named call parameter received a value of an incompatible type.
    #[error(
        "type mismatch for parameter `{parameter}` of `{callee}`: expected `{expected}`, found `{found}` (value `{actual}`)"
    )]
    ArgumentTypeMismatch {
        /// Fully-qualified callable name.
        callee: String,
        /// Source-level parameter name.
        parameter: String,
        /// Expected type, rendered via [`crate::render_ty`].
        expected: String,
        /// Found type, rendered via [`crate::render_ty`].
        found: String,
        /// Source-level rendering of the supplied value or expression.
        actual: String,
    },
    /// A method call could not be resolved to any known definition.
    #[error("no method named `{name}` found for type `{ty}`")]
    UnresolvedMethod {
        /// Receiver type.
        ty: String,
        /// Method name.
        name: String,
    },
    /// A binary or unary operator could not be resolved for the given
    /// operand types.
    #[error("cannot apply `{op}` to `{lhs}` and `{rhs}`")]
    UnresolvedOp {
        /// Operator symbol.
        op: String,
        /// Left-hand type.
        lhs: String,
        /// Right-hand type (for unary ops this is the operand).
        rhs: String,
    },
    /// An overloadable operator was applied to a user struct / enum that
    /// carries no impl of the operator's backing trait.
    #[error("cannot apply `{op}` to `{ty}`")]
    UnresolvedOpImpl {
        /// Operator symbol (`+`, `-`, `+=`, ...).
        op: String,
        /// Operator trait that would provide the operation (`Add`).
        trait_name: String,
        /// Impl method the operator dispatches to (`add`).
        method: String,
        /// The ADT operand's type.
        ty: String,
    },
    /// A `match` expression lacks coverage for one or more patterns.
    #[error("non-exhaustive patterns: {missing}")]
    NonExhaustiveMatch {
        /// Human-readable description of the missing patterns.
        missing: String,
    },
    /// A nominal struct or enum was destructured with an anonymous tuple
    /// pattern, which would bypass its declared name and field labels.
    #[error("destructuring `{ty}` requires its struct or variant name")]
    StructPatternNameRequired {
        /// The nominal type being destructured.
        ty: String,
    },
    /// A named struct was constructed with the removed field-literal form.
    #[error("struct `{name}` must be constructed with parentheses")]
    StructConstructorParenthesesRequired {
        /// The nominal struct name.
        name: String,
    },
    /// `value as T` was requested between two types that are not in
    /// the `as`-cast whitelist (non-primitive source, struct source,
    /// etc.).
    #[error("non-primitive cast: `{from}` as `{to}`")]
    InvalidCast {
        /// Source type.
        from: String,
        /// Target type.
        to: String,
    },
    /// Field access (`value.field`) on a type that has no such field.
    /// Splits two failure modes: `opaque` is true when the receiver's
    /// type is known but the checker has no field map for it (typical
    /// of dynamic stdlib types like `json::Value`); `opaque` is false
    /// when the type does have fields but `field` isn't one of them.
    #[error("type `{ty}` has no field `{field}`")]
    UnknownField {
        /// Receiver type.
        ty: String,
        /// Field name attempted.
        field: String,
        /// `true` when the receiver is opaque to the checker.
        opaque: bool,
    },
    /// A `Result<T, E>` expression was used as a statement without
    /// binding or propagating the value. SPEC §9: discarded Results
    /// are a compile error unless explicitly suppressed with `let _ =`.
    #[error("unused `Result` value - the `Err` variant may go unhandled")]
    DiscardedResult,
    /// An expression, type, or pattern nested past the type-checker's
    /// hard recursion limit. Emitted on adversarial input that
    /// survives parsing (rare, but the typechecker has its own
    /// guard to keep `cargo check`-style probes from crashing).
    #[error("expression nests beyond {limit} levels (consider rewriting with a helper)")]
    RecursionLimit {
        /// Recursion limit at which the error was raised.
        limit: u32,
    },
    /// A `type` alias expands to itself through a cycle (`type A = B;
    /// type B = A`), so it has no underlying type. Treated as
    /// `TyKind::Error` so downstream typing does not cascade.
    #[error("type alias `{name}` is cyclic - it expands to itself")]
    CyclicTypeAlias {
        /// Name of the alias at the point the cycle was detected.
        name: String,
    },
    /// A `#[derive(...)]` names a trait that synthesizes nothing: it is
    /// either automatic for value types (comparison / hashing / serde) or
    /// is implemented with `impl Trait for T`, not derived.
    #[error("`#[derive({name})]` is not supported")]
    UnsupportedDerive {
        /// The rejected derive name.
        name: String,
        /// Why it is unsupported and what to do instead.
        hint: String,
    },
    /// An integer literal overflows the value range of its declared
    /// type-suffix (e.g. `300i8`, `99999999999999999999i64`). Treated
    /// as `TyKind::Error` so downstream typing does not cascade.
    #[error("integer literal `{literal}` does not fit in `{ty}`")]
    IntLiteralOverflow {
        /// Source spelling of the literal, including suffix.
        literal: String,
        /// Name of the suffix-derived target type.
        ty: String,
    },
    /// A string escape that the parser accepted but cannot be
    /// validly decoded (out-of-range `\u{...}`, surrogate code
    /// point, non-ASCII `\x..`). Surfaced from the AST→typechecker
    /// boundary so that downstream lowering sees no malformed
    /// strings.
    #[error("invalid escape `{escape}` in string literal: {reason}")]
    InvalidEscape {
        /// Verbatim text of the rejected escape sequence.
        escape: String,
        /// Human-readable reason the escape is invalid.
        reason: String,
    },
    /// A trait bound `<T: Bound>` on a generic parameter names a
    /// trait the resolver does not know about. Catches typos
    /// (`Hashabel` for `Hashable`) at function declaration before
    /// the call site stumbles into a runtime "no method" error.
    #[error("unknown trait `{name}` in bound for parameter `{param}`")]
    UnknownTraitBound {
        /// Generic parameter the bound was attached to.
        param: String,
        /// Trait name as written.
        name: String,
    },
    /// A generic call instantiates a type parameter with a concrete type
    /// that does not implement a required trait bound.
    #[error("the trait bound `{ty}: {bound}` is not satisfied")]
    TraitBoundNotSatisfied {
        /// Concrete type supplied at the call site.
        ty: String,
        /// Trait the parameter is bound by.
        bound: String,
    },
    /// An enum declares more variants than the heap representation's
    /// one-byte discriminant can index.
    #[error("enum `{name}` has {count} variants; the maximum is 256")]
    TooManyVariants {
        /// Enum name.
        name: String,
        /// Declared variant count.
        count: usize,
    },
    /// A closure was passed to a std combinator whose signature the
    /// checker has no row for, leaving the closure's parameter types
    /// uninferrable. Without a concrete type the compiled tiers pin
    /// the parameter to i64 and a String/Error payload formats as a
    /// raw pointer, so this is a hard error instead of silent garbage.
    #[error(
        "cannot infer the parameter types of this closure passed to `{combinator}`; \
         annotate the parameter (e.g. `|x: String| ...`) or bind the payload through a typed `match`"
    )]
    ClosureParamUninferred {
        /// Qualified combinator path, e.g. `iter::map`.
        combinator: String,
    },
    /// `i128` / `u128` appeared in a type position, a literal
    /// suffix, or a cast target. The runtime's i64 value model has
    /// no 128-bit representation on any tier, so the checker
    /// rejects the types uniformly instead of letting the VM run
    /// them at silent 64-bit width.
    #[error("`{ty}` is not supported yet")]
    Int128Unsupported {
        /// Which of the two 128-bit spellings appeared.
        ty: String,
    },
    /// A std free function was used as a first-class value
    /// (`r.map_err(errors::new)`) but is not in the supported-set
    /// table, so the compiled tiers have no symbol to take the
    /// address of. Rejected uniformly on every tier rather than
    /// letting the VM accept what a native build cannot link.
    #[error(
        "std function `{path}` cannot be passed as a value on compiled tiers; \
         wrap it in a closure (e.g. `|x| {path}(x)`)"
    )]
    StdFnValueUnsupported {
        /// Qualified std path as written, e.g. `strings::repeat`.
        path: String,
    },
    /// A lazy iterator state was formatted or printed directly instead of
    /// being consumed by a terminal.
    #[error("iterator state cannot be formatted directly")]
    IteratorStateFormatted,
    /// A lazy iterator state was used after an adapter or terminal consumed it.
    #[error("iterator `{name}` was already consumed by `{operation}`")]
    IteratorStateConsumed {
        /// Local binding name.
        name: String,
        /// Operation that consumed it.
        operation: String,
    },
    /// `json::render` / `json::encode` was handed an enum value - most
    /// often a `Result` from `json::parse(..)` that is missing its `?`,
    /// or an `Option`. Enums have no JSON serialization; the VM
    /// tolerated the misuse while the compiled tiers silently produced
    /// an empty string, so the checker now rejects it uniformly.
    #[error(
        "`json::{op}` cannot serialize the enum type `{ty}`; unwrap it first \
         (e.g. with `?` or `match`), or use `to_json::<T>` for a struct"
    )]
    JsonNotSerializable {
        /// The `render` / `encode` function name as written.
        op: String,
        /// The rendered enum type, e.g. `Result<json::Value, errors::Error>`.
        ty: String,
    },
    /// A call supplied the wrong number of arguments for the callee's
    /// declared arity. The VM aborts on this and the LLVM backend
    /// silently drops or zero-fills the mismatched args, so the checker
    /// rejects it statically on every tier.
    #[error("`{callee}` takes {expected} argument(s) but {found} were supplied")]
    CallArityMismatch {
        /// Callee name as written.
        callee: String,
        /// Declared parameter count.
        expected: usize,
        /// Argument count at the call site.
        found: usize,
    },
    /// A path `Enum::Variant` named a variant the enum does not declare.
    /// The resolver leaves the path unresolved and the VM faults at
    /// runtime (GX0002); the checker rejects it where the enum is known.
    #[error("enum `{enum_name}` has no variant `{variant}`")]
    UnknownVariant {
        /// Enum type name.
        enum_name: String,
        /// Variant name as written.
        variant: String,
    },
    /// A method reached through a generic bound (`fn f<T: Pet>(p: &T)`)
    /// resolves only through a supertrait of the bound (`trait Pet:
    /// Animal`, `name` declared on `Animal`). The compiled tiers cannot
    /// lower supertrait-through-bound dispatch (SPEC §3.8), so it is
    /// rejected uniformly instead of miscompiling on the native tier.
    #[error(
        "method `{method}` on `{param}` comes from supertrait `{supertrait}` of bound `{bound}`; \
         supertrait methods through a generic bound are not supported"
    )]
    SupertraitMethodThroughBound {
        /// Generic parameter the receiver binds to.
        param: String,
        /// Method name as called.
        method: String,
        /// The directly-named bound trait.
        bound: String,
        /// The supertrait that actually declares the method.
        supertrait: String,
    },
    /// `value[index]` where `value`'s type cannot be indexed (only
    /// `[T]` / `[T; N]` / `Vec<T>` / `String` are). The VM faults at
    /// runtime (GX0001) and the compiled tier reads through the value
    /// as a base pointer (SIGSEGV), so it is rejected at check.
    #[error("type `{ty}` cannot be indexed")]
    NotIndexable {
        /// Receiver type as rendered.
        ty: String,
    },
    /// `value(args)` where `value`'s type is not callable (not a `fn`
    /// item, `fn(..)` pointer, or `Fn(..)` value). The VM faults
    /// (GX0001) and the compiled tier emits a call through a
    /// non-function symbol (build failure), so it is rejected at check.
    #[error("type `{ty}` is not callable")]
    NotCallable {
        /// Callee type as rendered.
        ty: String,
    },
    /// `value.N` where `value` is not a tuple, or `N` is past the
    /// tuple's arity. The VM faults (GX0004) and the compiled tier
    /// reads out-of-object memory (garbage / info leak), so it is
    /// rejected at check.
    #[error("type `{ty}` has no tuple field `.{index}`")]
    NoTupleField {
        /// Receiver type as rendered.
        ty: String,
        /// Positional index attempted.
        index: u64,
    },
    /// A `match` / `if let` arm patterns a `json::Value` scrutinee with a
    /// `json::Value::Object(..)` / `::Array(..)` / `::Int(..)` etc.
    /// constructor. `json::Value` is an opaque dynamic-document handle
    /// with no matchable discriminant across the tiers, so such a pattern
    /// silently falls through on the VM and faults on the compiled tiers;
    /// rejected at check so the surface stays sound and the dynamic
    /// accessor API is used instead.
    #[error("`json::Value::{variant}` cannot be used as a pattern")]
    JsonValuePatternUnsupported {
        /// The json variant named in the pattern (`Object`, `Int`, ...).
        variant: String,
    },
    /// `.downgrade()` was called on a by-value type with no runtime RC
    /// header (a scalar, `Option`/`Result`, or other packed value).
    /// `Weak<T>` is a non-owning pointer into a reference-counted
    /// allocation, so the compiled tiers read a header off the value's
    /// bits and fault (SIGSEGV); rejected at check.
    #[error("`.downgrade()` is not valid on `{ty}`")]
    WeakDowngradeNonRc {
        /// Rendered receiver type.
        ty: String,
    },
    /// A data-last `option::*` / `result::*` combinator was called at
    /// full arity with its trailing data argument not shaped as the
    /// module's payload type - most often the `Option`/`Result` passed
    /// first and the closure last. The runtime reads the closure slot
    /// as the data value and silently returns the `None`/`Err`
    /// fallback, so the checker rejects the call.
    #[error("`{combinator}` takes its `{shape}` argument last")]
    CombinatorDataArgMismatch {
        /// Qualified combinator path, e.g. `option::and_then`.
        combinator: String,
        /// The payload shape the data slot requires (`Option`/`Result`).
        shape: String,
    },
    /// An assignment (`=` or a compound `+=` / ...) targets a binding
    /// that was not declared `mut`. `let`/parameter bindings are
    /// immutable by default; a place rooted at one cannot be written.
    #[error("cannot assign to immutable binding `{name}`")]
    AssignToImmutable {
        /// Name of the immutable root binding.
        name: String,
    },
    /// An assignment targets a place reached through a shared `&T`
    /// reference. The reference binding's own `mut` qualifier cannot make
    /// its referent writable.
    #[error("cannot assign through shared reference `{name}`")]
    AssignThroughSharedReference {
        /// Name of the shared reference at the root of the place.
        name: String,
    },
    /// A mutable reference was requested for a place rooted at an immutable
    /// binding. Mutable references require a writable source place.
    #[error("cannot take a mutable reference to immutable binding `{name}`")]
    MutableReferenceToImmutable {
        /// Name of the immutable root binding.
        name: String,
    },
}

impl TypeError {
    /// Returns a short stable tag useful for snapshot tests.
    #[must_use]
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::TypeMismatch { .. } => "type-mismatch",
            Self::ArgumentTypeMismatch { .. } => "argument-type-mismatch",
            Self::UnresolvedMethod { .. } => "unresolved-method",
            Self::UnresolvedOp { .. } => "unresolved-op",
            Self::UnresolvedOpImpl { .. } => "unresolved-op-impl",
            Self::NonExhaustiveMatch { .. } => "non-exhaustive-match",
            Self::StructPatternNameRequired { .. } => "struct-pattern-name-required",
            Self::StructConstructorParenthesesRequired { .. } => {
                "struct-constructor-parentheses-required"
            }
            Self::InvalidCast { .. } => "invalid-cast",
            Self::UnknownField { .. } => "unknown-field",
            Self::DiscardedResult => "discarded-result",
            Self::RecursionLimit { .. } => "recursion-limit",
            Self::CyclicTypeAlias { .. } => "cyclic-type-alias",
            Self::UnsupportedDerive { .. } => "unsupported-derive",
            Self::IntLiteralOverflow { .. } => "int-literal-overflow",
            Self::InvalidEscape { .. } => "invalid-escape",
            Self::UnknownTraitBound { .. } => "unknown-trait-bound",
            Self::TraitBoundNotSatisfied { .. } => "trait-bound-not-satisfied",
            Self::TooManyVariants { .. } => "too-many-variants",
            Self::ClosureParamUninferred { .. } => "closure-param-uninferred",
            Self::Int128Unsupported { .. } => "int128-unsupported",
            Self::StdFnValueUnsupported { .. } => "std-fn-value-unsupported",
            Self::IteratorStateFormatted => "iterator-state-formatted",
            Self::IteratorStateConsumed { .. } => "iterator-state-consumed",
            Self::JsonNotSerializable { .. } => "json-not-serializable",
            Self::CallArityMismatch { .. } => "call-arity-mismatch",
            Self::UnknownVariant { .. } => "unknown-variant",
            Self::SupertraitMethodThroughBound { .. } => "supertrait-method-through-bound",
            Self::NotIndexable { .. } => "not-indexable",
            Self::NotCallable { .. } => "not-callable",
            Self::NoTupleField { .. } => "no-tuple-field",
            Self::JsonValuePatternUnsupported { .. } => "json-value-pattern-unsupported",
            Self::WeakDowngradeNonRc { .. } => "weak-downgrade-non-rc",
            Self::CombinatorDataArgMismatch { .. } => "combinator-data-arg-mismatch",
            Self::AssignToImmutable { .. } => "assign-to-immutable",
            Self::AssignThroughSharedReference { .. } => "assign-through-shared-reference",
            Self::MutableReferenceToImmutable { .. } => "mutable-reference-to-immutable",
        }
    }

    /// Stable error code used by the diagnostics framework.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TypeMismatch { .. } => "GT0001",
            Self::ArgumentTypeMismatch { .. } => "GT0001",
            Self::UnresolvedMethod { .. } => "GT0002",
            Self::UnresolvedOp { .. } | Self::UnresolvedOpImpl { .. } => "GT0003",
            Self::NonExhaustiveMatch { .. } => "GT0004",
            Self::StructPatternNameRequired { .. } => "GT0033",
            Self::StructConstructorParenthesesRequired { .. } => "GT0034",
            Self::InvalidCast { .. } => "GT0005",
            Self::UnknownField { .. } => "GT0006",
            Self::DiscardedResult => "GT0007",
            Self::RecursionLimit { .. } => "GT0008",
            Self::CyclicTypeAlias { .. } => "GT0024",
            Self::UnsupportedDerive { .. } => "GT0025",
            Self::IntLiteralOverflow { .. } => "GT0009",
            Self::InvalidEscape { .. } => "GT0010",
            Self::UnknownTraitBound { .. } => "GT0011",
            Self::TooManyVariants { .. } => "GT0012",
            Self::ClosureParamUninferred { .. } => "GT0013",
            Self::Int128Unsupported { .. } => "GT0014",
            Self::StdFnValueUnsupported { .. } => "GT0015",
            Self::IteratorStateFormatted => "GT0041",
            Self::IteratorStateConsumed { .. } => "GT0042",
            Self::JsonNotSerializable { .. } => "GT0016",
            Self::TraitBoundNotSatisfied { .. } => "GT0017",
            Self::CallArityMismatch { .. } => "GT0018",
            Self::UnknownVariant { .. } => "GT0019",
            Self::SupertraitMethodThroughBound { .. } => "GT0020",
            Self::NotIndexable { .. } => "GT0021",
            Self::NotCallable { .. } => "GT0022",
            Self::NoTupleField { .. } => "GT0023",
            Self::JsonValuePatternUnsupported { .. } => "GT0027",
            Self::WeakDowngradeNonRc { .. } => "GT0028",
            Self::CombinatorDataArgMismatch { .. } => "GT0029",
            Self::AssignToImmutable { .. } => "GT0030",
            Self::AssignThroughSharedReference { .. } => "GT0031",
            Self::MutableReferenceToImmutable { .. } => "GT0032",
        }
    }
}

/// Maps the most common `expected X, found Y` pairs to a one-line
/// "did you mean" hint. Pure string compare on the rendered types -
/// keeps the table small and avoids re-deriving structure here.
/// Attaches the GT0014 help + note to an `Int128Unsupported`
/// diagnostic. Split out of `to_diagnostic` to keep that match
/// within the line-count lint budget.
fn int128_diagnostic(
    out: gossamer_diagnostics::Diagnostic,
    ty: &str,
) -> gossamer_diagnostics::Diagnostic {
    out.with_help("use `i64` / `u64` or split the value into two 64-bit halves")
        .with_note(format!(
            "`{ty}` has no 128-bit runtime representation on any tier (VM, JIT, or \
             compiled tier); the VM would otherwise run it at silent 64-bit width"
        ))
}

fn mismatch_suggestion(expected: &str, found: &str) -> Option<String> {
    if expected.starts_with("Vec<") && found.starts_with("Iterator<") {
        return Some(
            "consume the iterator with `iter::collect(<expr>)` at this materialization boundary"
                .to_string(),
        );
    }
    if expected.starts_with("Iterator<") && found.starts_with("Vec<") {
        return Some(
            "start a lazy pipeline with an iterator constructor or pass the Vec to a lazy adapter"
                .to_string(),
        );
    }
    // String / &str
    if expected == "String" && found.ends_with("&str") {
        return Some("did you mean to call `.to_string()` on the value?".to_string());
    }
    if expected.ends_with("&str") && found == "String" {
        return Some("did you mean to call `.as_str()` on the value?".to_string());
    }
    // Numeric width - i32 ↔ i64, u32 ↔ u64, etc.
    let int_suffixes = [
        "i8", "i16", "i32", "i64", "i128", "u8", "u16", "u32", "u64", "u128", "isize", "usize",
    ];
    if int_suffixes.contains(&expected) && int_suffixes.contains(&found) {
        return Some(format!("cast explicitly with `<expr> as {expected}`"));
    }
    // T → Option<T>
    if let Some(inner) = expected
        .strip_prefix("Option<")
        .and_then(|s| s.strip_suffix('>'))
    {
        if inner == found {
            return Some(format!(
                "did you mean to wrap with `Some(<expr>)` to lift `{inner}` into `Option<{inner}>`?"
            ));
        }
    }
    // Result<T, _> → T (handler returned a Result, caller wanted the inner value)
    if found.starts_with("Result<") && !expected.starts_with("Result<") {
        return Some(
            "did you mean to propagate with `?` (`<expr>?`) to unwrap the `Result`?".to_string(),
        );
    }
    // &T vs T
    if let Some(rest) = found.strip_prefix('&') {
        if rest == expected {
            return Some(format!(
                "did you mean to dereference with `*<expr>` to get `{expected}`?"
            ));
        }
    }
    if let Some(rest) = expected.strip_prefix('&') {
        if rest == found {
            return Some(format!(
                "did you mean to take a reference with `&<expr>` to get `&{found}`?"
            ));
        }
    }
    None
}

impl TypeDiagnostic {
    /// Renders this diagnostic as a structured
    /// [`gossamer_diagnostics::Diagnostic`] for the new error frame.
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per diagnostic variant; splitting scatters the help text"
    )]
    pub fn to_diagnostic(&self) -> gossamer_diagnostics::Diagnostic {
        use gossamer_diagnostics::{Code, Diagnostic, Location};
        let location = Location::new(self.span.file, self.span);
        let title = format!("{}", self.error);
        let mut out =
            Diagnostic::error(Code(self.error.code()), title.clone()).with_primary(location, title);
        match &self.error {
            TypeError::TypeMismatch { expected, found } => {
                out = out.with_note(format!("expected `{expected}`, found `{found}`"));
                if let Some(suggestion) = mismatch_suggestion(expected, found) {
                    out = out.with_help(suggestion);
                }
            }
            TypeError::ArgumentTypeMismatch {
                callee,
                parameter,
                expected,
                found,
                actual,
            } => {
                out = out.with_note(format!(
                    "parameter `{parameter}` of `{callee}` expects `{expected}`, found `{found}` from `{actual}`"
                ));
                if let Some(suggestion) = mismatch_suggestion(expected, found) {
                    out = out.with_help(suggestion);
                }
            }
            TypeError::UnresolvedMethod { ty, name } => {
                out = if name == "set" && ty.starts_with("HashMap") {
                    out.with_help(format!("`{ty}` writes with `insert(key, value)`"))
                        .with_note(
                            "`set` is the `json::Value` field-update helper, not a map method",
                        )
                } else if name == "keys for aggregate HashMap keys" && ty.starts_with("HashMap") {
                    out.with_help(
                        "iterate the map with `for (key, value) in map.iter()` instead",
                    )
                    .with_note(
                        "`HashMap::keys()` is unavailable for struct, tuple, and array key types until aggregate key snapshots preserve their layout",
                    )
                } else {
                    out.with_help(format!("`{ty}` has no method named `{name}`"))
                        .with_note("check for a typo or an impl block missing from scope")
                };
            }
            TypeError::UnresolvedOp { op, lhs, rhs } => {
                out = out.with_note(format!(
                    "operator `{op}` requires matching operand types; got `{lhs}` and `{rhs}`"
                ));
            }
            TypeError::UnresolvedOpImpl {
                op,
                trait_name,
                method,
                ty,
            } => {
                let note = if trait_name == "Neg" {
                    format!("user types support unary `{op}` through an `impl {trait_name}`")
                } else {
                    format!(
                        "user types support `{op}` through an `impl {trait_name}`, dispatched on the left operand"
                    )
                };
                out = out.with_note(note).with_help(format!(
                    "implement `impl {trait_name} for {ty} {{ fn {method}(..) -> .. }}`"
                ));
            }
            TypeError::NonExhaustiveMatch { missing } => {
                out = out
                    .with_help(format!("add an arm for: {missing}"))
                    .with_note("match expressions must cover every possible value");
            }
            TypeError::StructPatternNameRequired { ty } => {
                out = out
                    .with_help(format!("write `{ty} {{ field, .. }}` with its declared fields"))
                    .with_note(
                        "parenthesized patterns destructure tuples only; structs are nominal and use named-field patterns",
                    );
            }
            TypeError::StructConstructorParenthesesRequired { name } => {
                out = out
                    .with_help(format!("write `{name}(...)` in declared field order"))
                    .with_note(
                        "struct declarations use braces; struct construction uses parentheses",
                    );
            }
            TypeError::InvalidCast { from, to } => {
                out = out
                    .with_help(
                        "`as` is restricted to numeric ↔ numeric, `bool`/`char` → integer, `u8` → `char`, and no-op same-type casts",
                    )
                    .with_note(format!("cannot cast `{from}` to `{to}`"));
            }
            TypeError::UnknownField { ty, field, opaque } => {
                out = unknown_field_diagnostic(out, ty, field, *opaque);
            }
            TypeError::TooManyVariants { .. } => {
                out = out.with_help(
                    "the heap enum discriminant is one byte; split the enum or                      group variants into nested enums.",
                );
            }
            TypeError::DiscardedResult => out = discarded_result_diagnostic(out),
            TypeError::TraitBoundNotSatisfied { ty, bound } => {
                out = out.with_help(format!(
                    "add `impl {bound} for {ty} {{ ... }}`, or pass a type that already implements `{bound}`"
                ));
            }
            TypeError::RecursionLimit { .. } => {
                out = out
                    .with_help("split the expression into smaller helpers")
                    .with_note(
                        "the typechecker bails out at a fixed depth to avoid a C-stack overflow",
                    );
            }
            TypeError::CyclicTypeAlias { name } => {
                out = out
                    .with_help(format!(
                        "`{name}` must eventually expand to a concrete type, not back to itself"
                    ))
                    .with_note("a cyclic alias has no underlying type, so every use is ill-typed");
            }
            TypeError::UnsupportedDerive { hint, .. } => {
                out = out.with_help(hint.clone());
            }
            TypeError::IntLiteralOverflow { literal, ty } => {
                out = out.with_help(format!("`{literal}` exceeds the range of `{ty}`"));
            }
            TypeError::InvalidEscape { escape, reason } => {
                out = out.with_help(format!("`{escape}` is not a valid escape: {reason}"));
            }
            TypeError::UnknownTraitBound { param, name } => {
                out = out
                    .with_help(format!(
                        "trait `{name}` is not declared anywhere; bound on `{param}` cannot be enforced",
                    ))
                    .with_note("check for a typo or import the trait into scope");
            }
            TypeError::ClosureParamUninferred { combinator } => {
                out = closure_param_diagnostic(out, combinator);
            }
            TypeError::Int128Unsupported { ty } => out = int128_diagnostic(out, ty),
            TypeError::StdFnValueUnsupported { path } => {
                out = std_fn_value_diagnostic(out, path);
            }
            TypeError::IteratorStateFormatted => {
                out = out.with_help(
                    "consume the iterator with `iter::collect(...)` before formatting it"
                        .to_string(),
                );
            }
            TypeError::IteratorStateConsumed { name, operation } => {
                out = out
                    .with_note(format!("`{operation}` takes ownership of `{name}`"))
                    .with_help("create a new iterator pipeline for another traversal".to_string());
            }
            TypeError::JsonNotSerializable { op, ty } => {
                out = json_not_serializable_diagnostic(out, op, ty);
            }
            TypeError::CallArityMismatch {
                callee, expected, ..
            } => out = arity_mismatch_diagnostic(out, callee, *expected),
            TypeError::UnknownVariant { enum_name, variant } => {
                out = out
                    .with_help(format!(
                        "check `{enum_name}::{variant}` against the declared variants"
                    ))
                    .with_note("an unknown variant resolves to nothing and faults at runtime");
            }
            TypeError::SupertraitMethodThroughBound {
                method,
                bound,
                supertrait,
                ..
            } => out = supertrait_method_diagnostic(out, method, bound, supertrait),
            TypeError::NotIndexable { .. }
            | TypeError::NotCallable { .. }
            | TypeError::NoTupleField { .. } => out = structural_use_diagnostic(out, &self.error),
            TypeError::JsonValuePatternUnsupported { .. } => {
                out = out
                    .with_help(
                        "read a `json::Value` with the dynamic accessors instead: \
                         `json::as_i64` / `json::as_f64` / `json::as_str` / `json::as_bool`, \
                         `json::is_null`, `json::get(&v, key)`, `json::at(&v, i)`, \
                         `json::keys(&v)`, `json::len(&v)`",
                    )
                    .with_note(
                        "`json::Value` is an opaque dynamic-document handle with no matchable \
                         discriminant, so a `json::Value::Variant(..)` pattern falls through \
                         on the VM and faults on the compiled tiers",
                    );
            }
            TypeError::CombinatorDataArgMismatch { combinator, shape } => {
                out = out
                    .with_help(format!(
                        "write `{combinator}(f, value)` or pipe the value in: \
                         `value |> {combinator}(f)`"
                    ))
                    .with_note(format!(
                        "the data slot is the last argument; a non-`{shape}` value there \
                         makes the runtime return the empty fallback instead of applying `f`"
                    ));
            }
            TypeError::WeakDowngradeNonRc { .. } => {
                out = out
                    .with_help(
                        "call `.downgrade()` on a reference-counted aggregate (a struct, \
                         payload-bearing enum, or other heap value) - the shape that \
                         participates in cycles a `Weak<T>` breaks",
                    )
                    .with_note(
                        "a scalar / `Option` / `Result` is a by-value word with no RC header, \
                         so `Weak` of it would read a header off the value's bits and fault \
                         on the compiled tiers",
                    );
            }
            TypeError::AssignToImmutable { name } => {
                out = out
                    .with_help(format!(
                        "declare it mutable: `let mut {name} = ...` (or `mut {name}` in the \
                         parameter list)"
                    ))
                    .with_note(
                        "bindings are immutable by default; only a `mut` place can be assigned",
                    );
            }
            TypeError::AssignThroughSharedReference { name } => {
                out = out
                    .with_help(format!(
                        "create `{name}` with `&mut` from a mutable place to write through it"
                    ))
                    .with_note("a shared `&T` reference permits reads but not writes");
            }
            TypeError::MutableReferenceToImmutable { name } => {
                out = out
                    .with_help(format!(
                        "declare the source mutable before borrowing it: `let mut {name} = ...`"
                    ))
                    .with_note("`&mut` requires a mutable place; it does not enforce exclusivity");
            }
        }
        out
    }
}

/// Attaches the GT0021 / GT0022 / GT0023 help + note. Split out of
/// `to_diagnostic` to keep that match within the line-count lint budget.
fn structural_use_diagnostic(
    out: gossamer_diagnostics::Diagnostic,
    error: &TypeError,
) -> gossamer_diagnostics::Diagnostic {
    match error {
        TypeError::NotIndexable { ty } => out
            .with_help(format!(
                "`{ty}` is not a `[T]`, `[T; N]`, `Vec<T>`, or `String`; index a user type through `impl Index for {ty}`"
            ))
            .with_note(
                "the VM faults (GX0001) and the compiled tier reads through the value as a pointer",
            ),
        TypeError::NotCallable { ty } => out
            .with_help(format!(
                "`{ty}` is not a function; only `fn` items, `fn(..)` pointers, and `Fn(..)` values can be called"
            ))
            .with_note(
                "the VM faults (GX0001) and the compiled tier emits a call through a non-function symbol",
            ),
        TypeError::NoTupleField { ty, index } => out
            .with_help(format!(
                "`{ty}` has no field `.{index}`; positional access works only on tuples within their arity"
            ))
            .with_note("the VM faults (GX0004) and the compiled tier reads out-of-object memory"),
        _ => out,
    }
}

/// Attaches the GT0007 help + note. Split out of `to_diagnostic` to keep
/// that match within the line-count lint budget.
fn discarded_result_diagnostic(
    out: gossamer_diagnostics::Diagnostic,
) -> gossamer_diagnostics::Diagnostic {
    out.with_help(
        "propagate the error with `?`, handle it with `match` / `if let`, \
         or explicitly discard with `let _ = <expr>`",
    )
    .with_note("SPEC §9: every `Result` value must be handled")
}

/// Attaches the GT0006 help. Split out of `to_diagnostic` to keep that
/// match within the line-count lint budget.
fn unknown_field_diagnostic(
    out: gossamer_diagnostics::Diagnostic,
    ty: &str,
    field: &str,
    opaque: bool,
) -> gossamer_diagnostics::Diagnostic {
    if opaque {
        out.with_help(format!(
            "`{ty}` has no named struct fields exposed to the language. \
             Use the type's methods (e.g. `value.get(\"{field}\")` for \
             `json::Value`) instead of named-field access."
        ))
    } else {
        out.with_help(format!(
            "check the spelling of `.{field}` and that the struct \
             definition for `{ty}` is in scope."
        ))
    }
}

/// Attaches the GT0018 help + note. Split out of `to_diagnostic` to keep
/// that match within the line-count lint budget.
fn arity_mismatch_diagnostic(
    out: gossamer_diagnostics::Diagnostic,
    callee: &str,
    expected: usize,
) -> gossamer_diagnostics::Diagnostic {
    out.with_help(format!(
        "`{callee}` is declared with {expected} parameter(s); pass exactly that many"
    ))
    .with_note(
        "the VM aborts on an arity mismatch and the native backend drops or \
         zero-fills the extra/missing arguments, so it is rejected at check",
    )
}

/// Attaches the GT0020 help + note. Split out of `to_diagnostic` to keep
/// that match within the line-count lint budget.
fn supertrait_method_diagnostic(
    out: gossamer_diagnostics::Diagnostic,
    method: &str,
    bound: &str,
    supertrait: &str,
) -> gossamer_diagnostics::Diagnostic {
    out.with_help(format!(
        "add `{method}` to bound `{bound}`, or bound the parameter on `{supertrait}` \
         directly (`<T: {supertrait}>`)"
    ))
    .with_note(
        "SPEC §3.8: a generic bound exposes only the named trait's own methods; \
         supertrait methods through the bound miscompile on the native tier",
    )
}

/// Attaches the GT0016 note + help. Split out of `to_diagnostic` to keep
/// that match within the line-count lint budget.
fn json_not_serializable_diagnostic(
    out: gossamer_diagnostics::Diagnostic,
    op: &str,
    ty: &str,
) -> gossamer_diagnostics::Diagnostic {
    out.with_note(format!(
        "`{ty}` is an enum, which has no JSON representation"
    ))
    .with_help(format!(
        "unwrap the value before encoding - e.g. `let v = …?` then \
             `json::{op}(&v)` - or build a `json::Value`"
    ))
}

/// Attaches the GT0013 help + note. Split out of `to_diagnostic` to
/// keep that match within the line-count lint budget.
fn closure_param_diagnostic(
    out: gossamer_diagnostics::Diagnostic,
    combinator: &str,
) -> gossamer_diagnostics::Diagnostic {
    out.with_help(
        "annotate the closure parameter with its concrete type \
         (e.g. `|x: String| ...`) or bind the payload through a typed `match`",
    )
    .with_note(format!(
        "`{combinator}` has no signature row in the checker, so the closure's \
         parameter type cannot be inferred; compiled tiers would otherwise read \
         heap payloads as raw integers"
    ))
}

/// Attaches the GT0015 help + note. Split out of `to_diagnostic` to
/// keep that match within the line-count lint budget.
fn std_fn_value_diagnostic(
    out: gossamer_diagnostics::Diagnostic,
    path: &str,
) -> gossamer_diagnostics::Diagnostic {
    out.with_help(format!(
        "wrap the call in a closure: `|x| {path}(x)` works on every tier"
    ))
    .with_note(
        "the VM models std functions as callable builtin values, but the compiled \
         tiers need a concrete runtime symbol; only the tabled supported set \
         (errors::new, strings::to_uppercase/.../trim, strconv::parse_i64/...) can be \
         passed directly",
    )
}

#[cfg(test)]
mod tests {
    use super::mismatch_suggestion;

    #[test]
    fn iterator_vec_mismatches_offer_migration_hints() {
        assert!(
            mismatch_suggestion("Vec<i64>", "Iterator<i64>")
                .is_some_and(|help| help.contains("iter::collect"))
        );
        assert!(
            mismatch_suggestion("Iterator<i64>", "Vec<i64>")
                .is_some_and(|help| help.contains("lazy adapter"))
        );
    }
}
