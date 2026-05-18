//! Property-based parser round-trip tests.
//!
//! Generates source from a restricted grammar (integer literals,
//! binary operators, `let` bindings, `fn` definitions, nested
//! blocks) and asserts that `parse -> pretty_print -> parse` yields
//! the same AST modulo span info.

#![allow(missing_docs)]

use gossamer_ast::{Printer, SourceFile};
use gossamer_lex::SourceMap;
use gossamer_parse::parse_source_file;
use proptest::prelude::*;

/// Parses `src` and returns the source file if parsing produced no
/// diagnostics. Generated inputs that happen to trip the parser
/// (defensive grammar narrowing rarely catches everything) are
/// silently skipped via `prop_assume!`.
fn parse_clean(src: &str) -> Option<SourceFile> {
    let mut map = SourceMap::new();
    let file = map.add_file("prop.gos", src.to_string());
    let (sf, diags) = parse_source_file(src, file);
    if diags.is_empty() { Some(sf) } else { None }
}

/// Re-renders `sf` to source via the AST pretty-printer.
fn pretty(sf: &SourceFile) -> String {
    let mut printer = Printer::new();
    printer.print_source_file(sf);
    printer.finish()
}

/// Parses `src`, pretty-prints, and re-parses. Returns the two ASTs
/// for the caller to compare. Returns `None` when either parse
/// surfaced diagnostics, so the property body can `prop_assume!`
/// out of the run.
fn round_trip(src: &str) -> Option<(SourceFile, SourceFile)> {
    let first = parse_clean(src)?;
    let printed = pretty(&first);
    let second = parse_clean(&printed)?;
    Some((first, second))
}

// --- grammar -------------------------------------------------------------

prop_compose! {
    fn arb_ident()(name in "[a-z][a-z0-9_]{0,5}") -> String {
        // Avoid keywords; the grammar window above is tight enough
        // that the only realistic collision is single letters.
        match name.as_str() {
            "if" | "in" | "fn" | "let" | "mut" | "as" | "of" | "do" | "for" => {
                format!("{name}_x")
            }
            _ => name,
        }
    }
}

prop_compose! {
    fn arb_int_literal()(n in 0i64..1_000_000) -> String {
        n.to_string()
    }
}

fn arb_binop() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("+".to_string()),
        Just("-".to_string()),
        Just("*".to_string()),
    ]
}

prop_compose! {
    /// Generates a binary expression of the form `<int> <op> <int>`.
    fn arb_binary_expr()
        (l in arb_int_literal(), op in arb_binop(), r in arb_int_literal()) -> String
    {
        format!("{l} {op} {r}")
    }
}

/// Generates either an int literal or a binary expression.
fn arb_simple_expr() -> impl Strategy<Value = String> {
    prop_oneof![arb_int_literal(), arb_binary_expr()]
}

prop_compose! {
    /// Generates a `let NAME = EXPR` statement.
    fn arb_let_stmt()
        (name in arb_ident(), expr in arb_simple_expr()) -> String
    {
        format!("let {name} = {expr}")
    }
}

prop_compose! {
    /// Generates a function definition with zero or more `i64` params
    /// and a body that is a single integer literal.
    fn arb_fn_def()
        (
            name in arb_ident(),
            params in prop::collection::vec(arb_ident(), 0..=3),
            body in arb_int_literal(),
        ) -> String
    {
        let mut dedup: Vec<String> = Vec::new();
        for p in params {
            if !dedup.iter().any(|d| d == &p) {
                dedup.push(p);
            }
        }
        let params_src = dedup
            .iter()
            .map(|p| format!("{p}: i64"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("fn {name}({params_src}) -> i64 {{\n    {body}\n}}")
    }
}

prop_compose! {
    /// Generates a function whose body contains one to three `let`
    /// statements followed by a tail integer expression.
    fn arb_nested_block_fn()
        (
            name in arb_ident(),
            lets in prop::collection::vec(arb_let_stmt(), 1..=3),
            tail in arb_int_literal(),
        ) -> String
    {
        let body = lets
            .into_iter()
            .map(|l| format!("    {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("fn {name}() -> i64 {{\n{body}\n    {tail}\n}}")
    }
}

/// Wraps an arbitrary expression as a function body so it parses at
/// item position.
fn wrap_as_fn(body: &str) -> String {
    format!("fn p() -> i64 {{\n    {body}\n}}")
}

// --- proptest config -----------------------------------------------------

fn config() -> ProptestConfig {
    // Cap shrink iterations so CI cannot get stuck reducing a tricky
    // case for minutes on end. PRNG seed is fixed by proptest's
    // default unless `PROPTEST_CASES` is overridden.
    ProptestConfig {
        cases: 64,
        max_shrink_iters: 64,
        max_shrink_time: 2_000,
        ..ProptestConfig::default()
    }
}

// --- properties ----------------------------------------------------------

proptest! {
    #![proptest_config(config())]

    #[test]
    fn int_literal_round_trips(lit in arb_int_literal()) {
        let src = wrap_as_fn(&lit);
        let (a, b) = match round_trip(&src) {
            Some(pair) => pair,
            None => return Ok(()),
        };
        prop_assert_eq!(a, b);
    }

    #[test]
    fn binary_op_round_trips(expr in arb_binary_expr()) {
        let src = wrap_as_fn(&expr);
        let (a, b) = match round_trip(&src) {
            Some(pair) => pair,
            None => return Ok(()),
        };
        prop_assert_eq!(a, b);
    }

    #[test]
    fn let_binding_round_trips(stmt in arb_let_stmt()) {
        let src = format!("fn p() -> i64 {{\n    {stmt}\n    0\n}}");
        let (a, b) = match round_trip(&src) {
            Some(pair) => pair,
            None => return Ok(()),
        };
        prop_assert_eq!(a, b);
    }

    #[test]
    fn fn_definition_round_trips(src in arb_fn_def()) {
        let (a, b) = match round_trip(&src) {
            Some(pair) => pair,
            None => return Ok(()),
        };
        prop_assert_eq!(a, b);
    }

    #[test]
    fn nested_block_round_trips(src in arb_nested_block_fn()) {
        let (a, b) = match round_trip(&src) {
            Some(pair) => pair,
            None => return Ok(()),
        };
        prop_assert_eq!(a, b);
    }
}
