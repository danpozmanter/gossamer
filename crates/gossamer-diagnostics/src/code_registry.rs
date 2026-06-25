//! Centralized registry of every diagnostic code Gossamer emits.
//!
//! Each entry pairs a stable code (e.g. `"GT0001"`) with a
//! one-paragraph explanation rendered by `gos explain CODE`. Front-end
//! crates emit the code; this module owns the explanation text.
//!
//! Invariants enforced by `tests/registry.rs`:
//! - every code emitted by the compiler appears here exactly once,
//! - codes are sorted alphabetically,
//! - explanation text is non-empty.

/// Stable code-to-explanation table consumed by `gos explain CODE` and
/// by `gossamer_lint::lint_explanation`. Sorted alphabetically by code.
pub const REGISTRY: &[(&str, &str)] = &[
    (
        "GL0001",
        "Declares a `let` binding whose name is never read.\n\
            Prefix the name with `_` to silence explicitly (e.g. `_tmp`)\n\
            when the binding is intentional but unused.",
    ),
    (
        "GL0002",
        "A `use` declaration whose imported name is never referenced.\n\
            Remove the import or reference the name in the file.",
    ),
    (
        "GL0003",
        "A binding marked `mut` that is never reassigned. Drop the `mut`\n\
            keyword.",
    ),
    (
        "GL0004",
        "`return expr` at the tail of a block is the same as writing `expr`\n\
            by itself. Prefer the tail form for symmetry with the rest of\n\
            the expression language.",
    ),
    (
        "GL0005",
        "`if cond { true } else { false }` is the same as `cond`. The\n\
            inverted form is `!cond`.",
    ),
    (
        "GL0006",
        "`x == true` reads worse than `x`, and `x == false` reads worse\n\
            than `!x`. Drop the literal.",
    ),
    (
        "GL0007",
        "A `match` with a single arm reads better as `if let PATTERN = ...`.\n\
            Single-arm `match` is almost always a half-written exhaustive match.",
    ),
    (
        "GL0008",
        "A `let` binding in the same block shadows an earlier one. Rename\n\
            one of them to make the data flow obvious.",
    ),
    (
        "GL0009",
        "`let _ = expr?` silently discards an error. Either handle the\n\
            `Err` branch explicitly or propagate with `?` so the caller sees it.",
    ),
    (
        "GL0010",
        "An empty `{}` block is almost always a mistake. Add an explicit\n\
            `()` tail if the block is intentional.",
    ),
    (
        "GL0011",
        "`panic!` inside `main` aborts without a clean exit code. Return a\n\
            `Result` from `main` and use `?` so the error propagates.",
    ),
    (
        "GL0012",
        "Calling `.clone()` on a literal or already-copied value is\n\
            redundant. Drop the call.",
    ),
    (
        "GL0013",
        "`!!x` collapses to `x` when `x: bool`. If the double negation is\n\
            intentional for truthiness coercion, use an explicit cast.",
    ),
    (
        "GL0014",
        "Assigning a variable to itself does nothing. The statement is\n\
            usually the residue of a refactor - remove it.",
    ),
    (
        "GL0015",
        "`todo!()` and `unimplemented!()` are placeholders, not shippable\n\
            expressions. Implement the branch before merging.",
    ),
    (
        "GL0016",
        "`if true { ... }` / `while false { ... }` - the branch is\n\
            decided at compile time. Drop the control-flow construct.",
    ),
    (
        "GL0017",
        "`let x = expr; x` at the tail of a block is just `expr`. Drop\n\
            the needless binding.",
    ),
    (
        "GL0018",
        "`if a { if b { ... } }` can be combined into\n\
            `if a && b { ... }`. Easier to scan.",
    ),
    (
        "GL0019",
        "Both branches of the `if` are identical. Drop the branch and\n\
            keep the body once.",
    ),
    (
        "GL0020",
        "`Foo { x: x }` is the same as the shorthand `Foo { x }`.",
    ),
    (
        "GL0021",
        "`if cond { return X } else { Y }` - the `else` is unreachable\n\
            fall-through. Un-nest the `else` body.",
    ),
    (
        "GL0022",
        "Comparing a value to itself is always `true` (for `==`, `<=`,\n\
            `>=`) or `false` (for `!=`, `<`, `>`). Use the constant.",
    ),
    (
        "GL0023",
        "`x + 0`, `x - 0`, `x * 1`, `x / 1` all equal `x`. The operation\n\
            adds nothing but noise.",
    ),
    (
        "GL0024",
        "`let x = ()` binds the unit value, which is almost never useful.\n\
            Drop the `let`.",
    ),
    (
        "GL0025",
        "Equality against a float literal is almost never what you want -\n\
            floating-point arithmetic rarely produces the exact bit pattern.\n\
            Compare `(x - y).abs() < eps` with an explicit tolerance.",
    ),
    (
        "GL0026",
        "`else {}` adds no information. Drop the else and let the `if`\n\
            stand alone.",
    ),
    (
        "GL0027",
        "`match b { true => ... false => ... }` is an `if` in disguise.\n\
            Rewrite as `if b { ... } else { ... }`.",
    ),
    (
        "GL0028",
        "`(x)` without a trailing comma is a needless pair of parens -\n\
            `x` reads the same. `(x,)` is a one-tuple and means something\n\
            different.",
    ),
    (
        "GL0029",
        "`!(a == b)` is just `a != b`. Prefer the direct operator.",
    ),
    (
        "GL0030",
        "Three or more nested `if / else if` layers are hard to skim.\n\
            Rewrite as `match` on the discriminant.",
    ),
    (
        "GL0031",
        "A literal range whose lower bound exceeds its upper bound is\n\
            empty. Swap the bounds or double-check the intent.",
    ),
    (
        "GL0032",
        "`\"a\" + \"b\"` can be written directly as `\"ab\"`. Let the\n\
            source reflect the final value.",
    ),
    ("GL0033", "`-(-x)` is `x`. The extra unary does nothing."),
    (
        "GL0034",
        "`if !cond { A } else { B }` scans better as `if cond { B }\n\
            else { A }`. Flip the branches and drop the `!`.",
    ),
    (
        "GL0035",
        "Concatenating an empty string literal is a no-op. Drop the\n\
            `\"\" +` or `+ \"\"`.",
    ),
    (
        "GL0036",
        "`println(\"\")` already writes a newline. Don't pass `\"\\n\"`\n\
            and don't call it twice to emit a blank line.",
    ),
    (
        "GL0037",
        "Two match arms share the same body. Either collapse them with\n\
            `|` alternation or extract the shared body into a helper.",
    ),
    (
        "GL0038",
        "Three consecutive statements `let tmp = a; a = b; b = tmp` swap\n\
            two bindings via a temporary. Prefer a destructuring assignment\n\
            once the language supports it, or at minimum document why the\n\
            swap is needed.",
    ),
    (
        "GL0039",
        "Two back-to-back assignments to the same place - the earlier\n\
            value is dead before it's read. Drop the first or consolidate\n\
            the logic into one statement.",
    ),
    (
        "GL0040",
        "Integer literals of five or more digits are easier to scan with\n\
            `_` as thousands separators: `1_000_000` instead of `1000000`.",
    ),
    (
        "GL0041",
        "`|x| f(x)` is a closure that just forwards to `f`. Pass `f`\n\
            directly.",
    ),
    (
        "GL0042",
        "An `if cond { } else { body }` is the same as `if !cond { body }`.\n\
            Invert the condition and drop the empty branch.",
    ),
    (
        "GL0043",
        "`match b { true => 1, false => 0 }` is an `if` in disguise that\n\
            happens to return an integer. Prefer `if b { 1 } else { 0 }`.",
    ),
    (
        "GL0044",
        "`fn f() -> () { ... }` is the same as `fn f() { ... }`. The\n\
            explicit `-> ()` is noise.",
    ),
    (
        "GL0045",
        "`let _: () = expr` annotates the binding with the unit type. If\n\
            `expr` was going to return `()` anyway, the annotation is noise.\n\
            If it wasn't, the annotation forces a coercion - use a plain\n\
            statement instead.",
    ),
    (
        "GL0046",
        "`match x { _ => expr }` always runs `expr` - the `match` adds\n\
            nothing. Drop the `match` (and add `let _ = x` if evaluating\n\
            the scrutinee has side effects).",
    ),
    (
        "GL0047",
        "`if (cond) { ... }` wraps the condition in a single-tuple\n\
            expression. Drop the parens: `if cond { ... }`.",
    ),
    (
        "GL0048",
        "`match () { ... }` has exactly one reachable arm. Drop the match\n\
            and run the body directly.",
    ),
    (
        "GL0049",
        "`panic()` with no argument leaves the post-mortem with nothing\n\
            to render. Always pass a brief explanation.",
    ),
    (
        "GL0050",
        "`loop {}` with no body busy-waits forever at 100% CPU. Add a\n\
            `break`, a `continue`, or replace with a real wait primitive.",
    ),
    (
        "GM0001",
        "Generic monomorphization received a type substitution that the\n\
                     compiler does not yet support - typically a generic parameter\n\
                     instantiated with a non-scalar (Vec, HashMap, struct). Track\n\
                     A's P8 widens this; in the meantime, instantiate the generic\n\
                     with a scalar (i64 / bool / f64) or write a non-generic\n\
                     specialisation.",
    ),
    (
        "GM0002",
        "A `match` arm is unreachable because an earlier arm already\n\
                     covers every value its pattern would match. Drop the dead\n\
                     arm or refine the earlier pattern so the later one becomes\n\
                     reachable.",
    ),
    (
        "GP0001",
        "The parser saw a token where it expected a different one.\n\
                     Check for missing punctuation, an unmatched delimiter, or an \n\
                     out-of-place keyword.",
    ),
    (
        "GP0002",
        "The parser reached end-of-file in the middle of a construct.\n\
                     Finish the expression, statement, or item - or remove it.",
    ),
    (
        "GP0003",
        "A balanced construct (block, tuple, array, string literal) was\n\
                     left unterminated. Add the matching closing delimiter.",
    ),
    (
        "GP0004",
        "Comparison operators like `==` / `!=` / `<` are not associative.\n\
                     Parenthesise the operands: `(a == b) && (b == c)`.",
    ),
    (
        "GP0005",
        "Range operators (`..`, `..=`) are not associative.\n\
                     Parenthesise the operands: `(a..b)..c`.",
    ),
    (
        "GP0006",
        "A braced struct literal in the scrutinee of `if`/`while`/`match`\n\
                     is ambiguous with the block that follows.\n\
                     Wrap the literal in `(...)`.",
    ),
    (
        "GP0007",
        "The right-hand side of `|>` must be a callable: a function\n\
                     reference, a method call (which receives the piped value as\n\
                     its last positional argument), or a closure.",
    ),
    (
        "GP0008",
        "Assignment (`=`, `+=`, …) only appears at statement position.\n\
                     If you need an expression, return the right-hand side\n\
                     directly.",
    ),
    ("GP0009", "An integer literal is required at this position."),
    ("GP0010", "A string literal is required at this position."),
    (
        "GP0011",
        "A tuple index must be a plain decimal integer (`p.0`, `p.1`).\n\
                     Hex, binary, or octal indices are not accepted.",
    ),
    (
        "GP0012",
        "A label identifier is required after the leading `'`.",
    ),
    (
        "GP0013",
        "An attribute is malformed. Accepted forms are `#[attr]`,\n\
                     `#[attr(args)]`, and `#[attr = value]`.",
    ),
    (
        "GP0014",
        "A `use` declaration could not be parsed. Check the path for\n\
                     stray punctuation or an unfinished brace list.",
    ),
    (
        "GP0015",
        "Two consecutive tokens formed something the parser does not\n\
                     recognise. Most often a missing operator or comma.",
    ),
    (
        "GP0016",
        "The `extern` keyword is reserved in 0.5.0 but has no\n\
                     source-level item form. Gossamer's FFI surface is the\n\
                     `[rust-bindings]` section of `project.toml` plus the\n\
                     `gossamer-binding` crate (see `docs_src/libraries.md`).\n\
                     Remove the `extern \"C\" { ... }` block or rewrite the\n\
                     binding as a Rust crate consumed via `[rust-bindings]`.",
    ),
    (
        "GP0017",
        "An expression nested past the parser's hard recursion limit.\n\
                     Surfaced as a guard against adversarial input; split the\n\
                     expression into smaller helpers so the parse tree stays\n\
                     within the limit.",
    ),
    (
        "GR0001",
        "A name used in source could not be resolved to a declaration.\n\
                     Check the spelling, whether a `use` brings the name into scope,\n\
                     and whether the item is visible at this location.",
    ),
    (
        "GR0002",
        "A path was found in the wrong namespace - for example a value\n\
                     where a type was expected, or a module where a value was\n\
                     expected. Re-check the import target.",
    ),
    (
        "GR0003",
        "Two items in the same module share a name. Rename one of them\n\
                     or move it into a distinct `mod`.",
    ),
    (
        "GR0004",
        "A `use` declaration imported the same name twice. Drop the\n\
                     duplicate or rename one of the imports with `use ... as ...`.",
    ),
    (
        "GR0005",
        "The `use` names a `std::` module path that does not exist.\n\
                     Every module has exactly one canonical path (e.g. JSON lives\n\
                     at `std::encoding::json`); check `gos doc` or the stdlib\n\
                     reference for the module's path.",
    ),
    (
        "GT0001",
        "The type checker could not reconcile two types it expected to\n\
                     match. The primary label shows the location of the mismatch;\n\
                     the `note:` line names the conflicting types.",
    ),
    (
        "GT0002",
        "The type checker could not find a method with the supplied\n\
                     name on the receiver type. Check for a typo, a missing `use`,\n\
                     or a trait impl that lives in an unreachable module.",
    ),
    (
        "GT0003",
        "An operator (`+`, `*`, `==`, …) was applied to a type that does\n\
                     not implement it. Either change the operand type or implement\n\
                     the trait that backs the operator.",
    ),
    (
        "GT0004",
        "A `match` expression does not cover every possible value. Add\n\
                     an arm for the pattern(s) listed under `help:`.",
    ),
    (
        "GT0005",
        "The `as` cast is restricted to a whitelist of conversions:\n\
                     numeric <-> numeric, `bool`/`char` -> integer, `u8` -> `char`,\n\
                     and same-type no-ops. Struct / enum / String sources are\n\
                     rejected. Use a conversion method when you need serialisation;\n\
                     `as` does not run code.",
    ),
    (
        "GT0006",
        "A struct field access (`x.field`) referenced a name that the\n\
                     receiver type does not declare. Check the field name or the\n\
                     receiver's actual type - generics and inference often resolve\n\
                     this once the surrounding code is more constrained.",
    ),
    (
        "GT0007",
        "A `Result<T, E>` expression was used as a statement without its\n\
                     value being handled. If the operation failed the error is\n\
                     silently ignored, which is almost always a bug.\n\n\
                     Three ways to fix this:\n\
                     - Propagate with `?`: `do_something()?` (requires the\n\
                       enclosing function to return `Result`).\n\
                     - Match explicitly: `match do_something() { Ok(v) => …, Err(e) => … }`.\n\
                     - Acknowledge and discard: `let _ = do_something()` - this\n\
                       silences GT0007 but leaves the error unhandled; only\n\
                       appropriate when the operation is best-effort.\n\n\
                     SPEC §9 requires every `Result` value to be handled.",
    ),
    (
        "GT0008",
        "An expression, type, or pattern nested past the type-checker's\n\
                     hard recursion limit. Emitted on adversarial input that\n\
                     survives parsing; rewrite the construct with smaller helpers\n\
                     so the typechecker stays within its guard.",
    ),
    (
        "GT0009",
        "An integer literal overflows the value range of its declared\n\
                     type-suffix (e.g. `300i8`, `99999999999999999999i64`). The\n\
                     literal is treated as `TyKind::Error` so downstream typing\n\
                     does not cascade; pick a wider suffix or shrink the literal.",
    ),
    (
        "GT0010",
        "A string escape that the parser accepted but cannot be validly\n\
                     decoded (out-of-range `\\u{...}`, surrogate code point,\n\
                     non-ASCII `\\x..`). Fix the escape so the resulting Unicode\n\
                     scalar value is in range.",
    ),
    (
        "GT0011",
        "A `<T: Bound>` clause names a trait the resolver does not know\n\
                     about. Check the spelling, or bring the trait into scope so\n\
                     the bound can be enforced.",
    ),
    (
        "GT0012",
        "An enum declares more variants than the one-byte heap\n\
                     discriminant can index (256). Split the enum or group\n\
                     variants into nested enums.",
    ),
    (
        "GT0013",
        "A closure was passed to a std combinator the checker has no\n\
                     signature row for, so its parameter type cannot be inferred.\n\
                     Annotate the parameter (`|x: String| ...`) or bind the\n\
                     payload through a typed `match`.",
    ),
    (
        "GT0014",
        "`i128` / `u128` have no 128-bit runtime representation on any\n\
                     tier. Use `i64` / `u64`, or split the value into two 64-bit\n\
                     halves.",
    ),
    (
        "GT0015",
        "A std free function was used as a first-class value but is not\n\
                     in the supported table; the compiled tiers have no symbol to\n\
                     take the address of. Wrap the call in a closure: `|x| f(x)`.",
    ),
    (
        "GT0016",
        "`json::render` / `json::encode` was handed an enum value (often\n\
                     a `Result` missing its `?`). Enums have no JSON form; unwrap\n\
                     first, or use `to_json::<T>` for a struct.",
    ),
    (
        "GT0017",
        "A generic call instantiates a type parameter with a concrete\n\
                     type that does not implement a required trait bound. Add the\n\
                     `impl Trait for Type` or pass a type that already does.",
    ),
    (
        "GT0018",
        "A call supplied the wrong number of arguments for the callee's\n\
                     declared arity. The VM aborts on this and the native backend\n\
                     drops or zero-fills the mismatched arguments, so it is\n\
                     rejected at check time. Pass exactly as many arguments as the\n\
                     function declares parameters.",
    ),
    (
        "GT0019",
        "A path `Enum::Variant` named a variant the enum does not\n\
                     declare. The resolver leaves the path unresolved and the\n\
                     program faults at runtime; check the variant spelling against\n\
                     the enum declaration.",
    ),
    (
        "GT0020",
        "A method reached through a generic bound resolves only through a\n\
                     supertrait of that bound (e.g. `fn f<T: Pet>(p: &T)` calling a\n\
                     method declared on `Animal` where `trait Pet: Animal`). The\n\
                     compiled tiers cannot lower supertrait-through-bound dispatch\n\
                     (SPEC §3.8); add the method to the named bound, or bound the\n\
                     parameter on the supertrait directly.",
    ),
    (
        "GT0021",
        "`value[index]` was used on a type that cannot be indexed. Only\n\
                     `[T]`, `[T; N]`, `Vec<T>`, and `String` support indexing. The\n\
                     VM faults (GX0001) and the compiled tier reads through the\n\
                     value as a base pointer (segfault), so it is rejected at check.",
    ),
    (
        "GT0022",
        "`value(args)` was used on a type that is not callable. Only `fn`\n\
                     items, `fn(..)` pointers, and `Fn(..)` values can be called.\n\
                     The VM faults (GX0001) and the compiled tier emits a call\n\
                     through a non-function symbol, so it is rejected at check.",
    ),
    (
        "GT0023",
        "`value.N` positional access was used on a non-tuple, or `N` is\n\
                     past the tuple's arity. The VM faults (GX0004) and the compiled\n\
                     tier reads out-of-object memory, so it is rejected at check.",
    ),
    (
        "GX0001",
        "A runtime value had the wrong shape for the operation. The\n\
                     interpreter catches this at execution time; the native\n\
                     backend aborts with the same code.",
    ),
    (
        "GX0002",
        "A name resolved at parse/resolve time to nothing callable at\n\
                     runtime. Usually means a stdlib builtin is not wired into the\n\
                     execution path that reached the call.",
    ),
    (
        "GX0003",
        "A call supplied the wrong number of arguments for the callee's\n\
                     declared arity. Fix the call site or update the declaration.",
    ),
    (
        "GX0004",
        "An arithmetic operation overflowed, divided by zero, or produced\n\
                     a value outside the representable range.",
    ),
    (
        "GX0005",
        "Explicit `panic!(...)` or an assertion failure aborted the\n\
                     program. Wrap the fallible operation in a `Result` path if the\n\
                     failure is recoverable.",
    ),
    (
        "GX0006",
        "A `match` expression failed to match any arm at runtime. The\n\
                     exhaustiveness checker catches most of these statically; a\n\
                     `GX0006` at runtime means a refinement check slipped through.",
    ),
    (
        "GX0007",
        "The execution path (interpreter or native) does not yet\n\
                     implement the construct reached. File the example and use\n\
                     the other path in the meantime.",
    ),
    (
        "GX0008",
        "The goroutine exceeded the VM's maximum call depth (40 frames).\n\
                     Each interpreted Gossamer frame adds a large pair of Rust stack\n\
                     frames (apply + run); the 8 MB OS thread stack can safely hold\n\
                     around 40 such pairs in a debug build before overflowing.\n\
                     Direct or mutual recursion without a reachable base case is the\n\
                     most common cause. Add a terminating condition, convert to an\n\
                     iterative loop, or use `gos build` where the native codegen\n\
                     produces standard call instructions the OS can grow to handle.",
    ),
];

/// Returns the explanation text for `code`, or `None` when the code
/// is not in the registry. Lookup is case-sensitive on the canonical
/// upper-case form; callers should pre-uppercase user input.
#[must_use]
pub fn explain(code: &str) -> Option<&'static str> {
    REGISTRY
        .binary_search_by_key(&code, |(k, _)| *k)
        .ok()
        .map(|i| REGISTRY[i].1)
}

/// Returns every code currently registered, in registry order.
pub fn codes() -> impl Iterator<Item = &'static str> {
    REGISTRY.iter().map(|(k, _)| *k)
}
