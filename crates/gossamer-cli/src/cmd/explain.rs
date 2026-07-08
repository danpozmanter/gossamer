//! `gos explain CODE` - describes a diagnostic (parser / resolver
//! / type / monomorph / runtime) or a lint code.
//!
//! Lookup table: built-in `GP/GR/GT/GM/GX####` codes have their
//! own short prose here; lint `GL####` codes are translated to the
//! lint id and explained by the lint registry.

use anyhow::{Result, anyhow};

/// Entry point for `gos explain CODE`.
pub(crate) fn run(code: &str) -> Result<()> {
    let upper = code.to_ascii_uppercase();
    if let Some(text) = diagnostic_explanation(&upper) {
        println!("{upper}\n\n{text}");
        return Ok(());
    }
    if let Some(id) = lint_id_for_code(&upper) {
        if let Some(text) = gossamer_lint::lint_explanation(id) {
            println!("{upper} ({id})\n\n{text}");
            return Ok(());
        }
    }
    Err(anyhow!(
        "no explanation registered for `{upper}`. See docs/diagnostics.md for the code catalogue."
    ))
}

#[allow(
    clippy::too_many_lines,
    reason = "flat lookup table; splitting hurts grep-ability"
)]
fn diagnostic_explanation(code: &str) -> Option<&'static str> {
    Some(match code {
        "GP0001" => {
            "The parser saw a token where it expected a different one.\n\
                     Check for missing punctuation, an unmatched delimiter, or an \n\
                     out-of-place keyword."
        }
        "GP0002" => {
            "The parser reached end-of-file in the middle of a construct.\n\
                     Finish the expression, statement, or item - or remove it."
        }
        "GP0003" => {
            "A balanced construct (block, tuple, array, string literal) was\n\
                     left unterminated. Add the matching closing delimiter."
        }
        "GP0004" => {
            "Comparison operators like `==` / `!=` / `<` are not associative.\n\
                     Parenthesise the operands: `(a == b) && (b == c)`."
        }
        "GR0001" => {
            "A name used in source could not be resolved to a declaration.\n\
                     Check the spelling, whether a `use` brings the name into scope,\n\
                     and whether the item is visible at this location."
        }
        "GR0002" => {
            "A path was found in the wrong namespace - for example a value\n\
                     where a type was expected, or a module where a value was\n\
                     expected. Re-check the import target."
        }
        "GR0003" => {
            "Two items in the same module share a name. Rename one of them\n\
                     or move it into a distinct `mod`."
        }
        "GR0004" => {
            "A `use` declaration imported the same name twice. Drop the\n\
                     duplicate or rename one of the imports with `use ... as ...`."
        }
        "GR0005" => {
            "The `use` names a `std::` module path that does not exist.\n\
                     Every module has exactly one canonical path (e.g. JSON\n\
                     lives at `std::encoding::json`); check `gos doc` or the\n\
                     stdlib reference for the module's path."
        }
        "GT0001" => {
            "The type checker could not reconcile two types it expected to\n\
                     match. The primary label shows the location of the mismatch;\n\
                     the `note:` line names the conflicting types."
        }
        "GT0002" => {
            "The type checker could not find a method with the supplied\n\
                     name on the receiver type. Check for a typo, a missing `use`,\n\
                     or a trait impl that lives in an unreachable module."
        }
        "GT0003" => {
            "An operator (`+`, `*`, `==`, …) was applied to a type that does\n\
                     not implement it. Either change the operand type or implement\n\
                     the trait that backs the operator."
        }
        "GT0004" => {
            "A `match` expression does not cover every possible value. Add\n\
                     an arm for the pattern(s) listed under `help:`."
        }
        "GT0005" => {
            "The `as` cast is restricted to a whitelist of conversions:\n\
                     numeric <-> numeric, `bool`/`char` -> integer, `u8` -> `char`,\n\
                     and same-type no-ops. Struct / enum / String sources are\n\
                     rejected. Use a conversion method when you need serialisation;\n\
                     `as` does not run code."
        }
        "GT0006" => {
            "A struct field access (`x.field`) referenced a name that the\n\
                     receiver type does not declare. Check the field name or the\n\
                     receiver's actual type - generics and inference often resolve\n\
                     this once the surrounding code is more constrained."
        }
        "GT0007" => {
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
                     SPEC §9 requires every `Result` value to be handled."
        }
        "GT0013" => {
            "A closure was passed to a std combinator the type checker has\n\
                     no signature row for, so the closure's parameter type cannot\n\
                     be inferred. The VM would run it, but the compiled tiers pin\n\
                     untyped closure params to i64 and read String / Error heap\n\
                     payloads as raw integers - so the program is rejected\n\
                     uniformly instead. Annotate the closure parameter with its\n\
                     concrete type (`|x: String| ...`) or bind the payload through\n\
                     a typed `match`."
        }
        "GT0014" => {
            "`i128` / `u128` have no 128-bit runtime representation on any\n\
                     tier: the runtime integer model is i64-word-sized (VM, JIT,\n\
                     and LLVM), so a 128-bit binding would silently truncate to\n\
                     64-bit width. The checker rejects the type instead. Use\n\
                     `i64` / `u64`, or split the value into two 64-bit halves."
        }
        "GT0015" => {
            "A std free function was used as a first-class value (for\n\
                     example `r.map_err(strings::repeat)`) but is not in the\n\
                     supported table. The VM models every std builtin as a\n\
                     callable value, but the compiled tiers need a concrete\n\
                     runtime symbol to take the address of; only the tabled set\n\
                     (errors::new, the strings:: case/trim helpers, the\n\
                     strconv:: parse/format helpers) can be passed directly.\n\
                     Wrap the call in a closure instead: `|x| module::fn(x)`\n\
                     works on every tier."
        }
        "GT0017" => {
            "A generic call instantiates a type parameter with a concrete\n\
                     type that does not implement a required trait bound. Add the\n\
                     `impl Trait for Type { ... }`, or pass a type that already\n\
                     implements the bound."
        }
        "GT0018" => {
            "A call supplied the wrong number of arguments for the callee's\n\
                     declared arity. The VM aborts on this and the native backend\n\
                     drops or zero-fills the mismatched arguments, so the checker\n\
                     rejects it on every tier. Pass exactly as many arguments as\n\
                     the function declares parameters."
        }
        "GT0019" => {
            "A path `Enum::Variant` named a variant the enum does not\n\
                     declare. The resolver leaves the path unresolved and the\n\
                     program faults at runtime (GX0002). Check the variant\n\
                     spelling against the enum declaration."
        }
        "GT0020" => {
            "A method reached through a generic bound resolves only through\n\
                     a supertrait of that bound - e.g. `fn f<T: Pet>(p: &T)`\n\
                     calling a method declared on `Animal` where\n\
                     `trait Pet: Animal`. The compiled tiers cannot lower\n\
                     supertrait-through-bound dispatch (SPEC §3.8), so it is\n\
                     rejected uniformly. Add the method to the named bound, or\n\
                     bound the parameter on the supertrait directly\n\
                     (`<T: Animal>`)."
        }
        "GT0024" => {
            "A `type` alias expands to itself through a cycle\n\
                     (`type A = B; type B = A`), so it has no underlying type.\n\
                     Every use of the alias is ill-typed. Break the cycle so the\n\
                     alias eventually names a concrete type."
        }
        "GT0025" => {
            "A `#[derive(...)]` named a trait that synthesizes nothing.\n\
                     Gossamer structs and enums are value types: they compare,\n\
                     order, hash, and copy by value automatically, so `==`, `<`,\n\
                     HashMap keys, `let b = a`, and `a.clone()` work with no\n\
                     derive. Only Debug, Default, PartialEq, Eq, PartialOrd, and\n\
                     Ord are derivable. Clone, Hash, Copy, Display, and Serialize\n\
                     are automatic; From / Into / the operator traits are written\n\
                     with `impl Trait for T`."
        }
        "GT0027" => {
            "A `match` / `if let` / `while let` arm patterns a `json::Value`\n\
                     scrutinee with a `json::Value::Object(..)` / `::Array(..)` /\n\
                     `::Int(..)` (etc.) constructor. `json::Value` is an opaque\n\
                     dynamic-document handle with no matchable discriminant, so the\n\
                     pattern silently falls through on the VM and faults on the\n\
                     compiled tiers. Rejected at check so the three tiers agree.\n\
                     Read the document with the dynamic accessors instead:\n\
                     `json::as_i64` / `json::as_f64` / `json::as_str` /\n\
                     `json::as_bool`, `json::is_null`, `json::get(&v, key)`,\n\
                     `json::at(&v, i)`, `json::keys(&v)`, `json::len(&v)`."
        }
        "GT0028" => {
            "`.downgrade()` was called on a by-value type with no runtime RC\n\
                     header - a scalar (`i64` / `bool` / ...), an `Option` /\n\
                     `Result`, or another packed value. `Weak<T>` is a non-owning\n\
                     pointer into a reference-counted allocation: the compiled\n\
                     tiers read a header off the value's bits and fault (SIGSEGV)\n\
                     while the VM returns a bogus handle, so it is rejected at\n\
                     check. Downgrade a reference-counted aggregate instead - a\n\
                     struct or payload-bearing enum, the shape that participates in\n\
                     the cycles a `Weak<T>` is there to break."
        }
        "GM0001" => {
            "Generic monomorphization received a type substitution that the\n\
                     compiler does not yet support - typically a generic parameter\n\
                     instantiated with a non-scalar (Vec, HashMap, struct). Track\n\
                     A's P8 widens this; in the meantime, instantiate the generic\n\
                     with a scalar (i64 / bool / f64) or write a non-generic\n\
                     specialisation."
        }
        "GM0003" => {
            "A value allocated inside an `arena { }` block is used after the\n\
                     block exits. The arena frees its memory wholesale at the\n\
                     closing brace, so the reference would dangle - a use-after-free.\n\
                     The check rejects storing the value into a binding that outlives\n\
                     the block, pushing it into an outer container, sending it on a\n\
                     channel, returning it, breaking out of an enclosing loop,\n\
                     capturing it in a goroutine/closure, or passing it to a function\n\
                     that may stash it. Keep only a scalar or already-outside summary,\n\
                     or allocate the value before the block. The raw\n\
                     `runtime::arena_push/pop` primitive is left unchecked."
        }
        "GP0005" => {
            "Range operators (`..`, `..=`) are not associative.\n\
                     Parenthesise the operands: `(a..b)..c`."
        }
        "GP0006" => {
            "A braced struct literal in the scrutinee of `if`/`while`/`match`\n\
                     is ambiguous with the block that follows.\n\
                     Wrap the literal in `(...)`."
        }
        "GP0007" => {
            "The right-hand side of `|>` must be a callable: a function\n\
                     reference, a method call (which receives the piped value as\n\
                     its last positional argument), or a closure."
        }
        "GP0008" => {
            "Assignment (`=`, `+=`, …) only appears at statement position.\n\
                     If you need an expression, return the right-hand side\n\
                     directly."
        }
        "GP0009" => "An integer literal is required at this position.",
        "GP0010" => "A string literal is required at this position.",
        "GP0011" => {
            "A tuple index must be a plain decimal integer (`p.0`, `p.1`).\n\
                     Hex, binary, or octal indices are not accepted."
        }
        "GP0012" => "A label identifier is required after the leading `'`.",
        "GP0013" => {
            "An attribute is malformed. Accepted forms are `#[attr]`,\n\
                     `#[attr(args)]`, and `#[attr = value]`."
        }
        "GP0014" => {
            "A `use` declaration could not be parsed. Check the path for\n\
                     stray punctuation or an unfinished brace list."
        }
        "GP0015" => {
            "Two consecutive tokens formed something the parser does not\n\
                     recognise. Most often a missing operator or comma."
        }
        "GP0016" => {
            "The `extern` keyword is reserved but has no\n\
                     source-level item form. Gossamer's FFI surface is the\n\
                     `[rust-bindings]` section of `project.toml` plus the\n\
                     `gossamer-binding` crate (see `docs_src/libraries.md`).\n\
                     Remove the `extern \"C\" { ... }` block or rewrite the\n\
                     binding as a Rust crate consumed via `[rust-bindings]`."
        }
        "GX0001" => {
            "A runtime value had the wrong shape for the operation. The\n\
                     interpreter catches this at execution time; the native\n\
                     backend aborts with the same code."
        }
        "GX0002" => {
            "A name resolved at parse/resolve time to nothing callable at\n\
                     runtime. Usually means a stdlib builtin is not wired into the\n\
                     execution path that reached the call."
        }
        "GX0003" => {
            "A call supplied the wrong number of arguments for the callee's\n\
                     declared arity. Fix the call site or update the declaration."
        }
        "GX0004" => {
            "An arithmetic operation overflowed, divided by zero, or produced\n\
                     a value outside the representable range."
        }
        "GX0005" => {
            "Explicit `panic!(...)` or an assertion failure aborted the\n\
                     program. Wrap the fallible operation in a `Result` path if the\n\
                     failure is recoverable."
        }
        "GX0006" => {
            "A `match` expression failed to match any arm at runtime. The\n\
                     exhaustiveness checker catches most of these statically; a\n\
                     `GX0006` at runtime means a refinement check slipped through."
        }
        "GX0007" => {
            "The execution path (interpreter or native) does not yet\n\
                     implement the construct reached. File the example and use\n\
                     the other path in the meantime."
        }
        "GX0008" => {
            "The goroutine ran out of stack. Recursion is bounded two ways: a\n\
                     call-depth cap (40 frames in a debug build, 512 in release) and\n\
                     a byte-budget guard that trips when the native stack grows past\n\
                     its reserve (64 MiB on a VM / goroutine worker thread, less the\n\
                     256 KiB unwind margin). Whichever is reached first raises this\n\
                     clean error rather than faulting on the guard page. Compiled\n\
                     `gos build` binaries install the same guard and report a\n\
                     `stack overflow ... aborting` message instead of a raw SIGSEGV.\n\
                     Direct or mutual recursion without a reachable base case is the\n\
                     most common cause. Add a terminating condition, convert to an\n\
                     iterative loop, or use `gos build`, where native code lets the\n\
                     OS grow the stack for genuinely deep (bounded) recursion."
        }
        _ => return None,
    })
}

fn lint_id_for_code(code: &str) -> Option<&'static str> {
    match code {
        "GL0001" => Some("unused_variable"),
        "GL0002" => Some("unused_import"),
        "GL0003" => Some("unused_mut_variable"),
        "GL0004" => Some("needless_return"),
        "GL0005" => Some("needless_bool"),
        "GL0006" => Some("comparison_to_bool_literal"),
        "GL0007" => Some("single_match"),
        "GL0008" => Some("shadowed_binding"),
        "GL0009" => Some("unchecked_result"),
        "GL0010" => Some("empty_block"),
        "GL0011" => Some("panic_in_main"),
        "GL0012" => Some("redundant_clone"),
        "GL0013" => Some("double_negation"),
        "GL0014" => Some("self_assignment"),
        "GL0015" => Some("todo_macro"),
        _ => None,
    }
}
