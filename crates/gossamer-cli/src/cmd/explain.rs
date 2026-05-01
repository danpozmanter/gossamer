//! `gos explain CODE` — describes a diagnostic (parser / resolver
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
                     Finish the expression, statement, or item — or remove it."
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
            "A path was found in the wrong namespace — for example a value\n\
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
                     receiver's actual type — generics and inference often resolve\n\
                     this once the surrounding code is more constrained."
        }
        "GM0001" => {
            "Generic monomorphization received a type substitution that the\n\
                     compiler does not yet support — typically a generic parameter\n\
                     instantiated with a non-scalar (Vec, HashMap, struct). Track\n\
                     A's P8 widens this; in the meantime, instantiate the generic\n\
                     with a scalar (i64 / bool / f64) or write a non-generic\n\
                     specialisation."
        }
        "GP0005" => {
            "A function declared as `extern \"C\"` had a body. `extern \"C\"`\n\
                     items are foreign declarations; remove the body or drop the\n\
                     `extern` keyword."
        }
        "GP0006" => {
            "An attribute argument was malformed. Attribute syntax accepts\n\
                     `#[attr]`, `#[attr(args)]`, or `#[attr = value]`."
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
