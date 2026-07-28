//! Parser robustness pin-tests.
//!
//! `smoke.rs` covers happy-path parsing. This file pins the
//! "parser must never panic" invariant for adversarial inputs:
//! malformed strings, deeply nested expressions, weird Unicode,
//! truncated programs. The regression class is "panic in the
//! pipeline" - a parser that bails with a structured diagnostic
//! is fine, but `unwrap()`-ing on missing tokens or unexpected
//! shapes shouldn't crash the process.

#![allow(missing_docs)]

use gossamer_lex::SourceMap;
use gossamer_parse::parse_source_file;

fn parse_does_not_panic(source: &str) {
    let mut map = SourceMap::new();
    let file = map.add_file("probe.gos", source.to_string());
    // We deliberately ignore both the AST and diagnostics - the
    // shape we care about is "didn't panic". Diagnostics are
    // expected for ill-formed inputs.
    let (_sf, _diags) = parse_source_file(source, file);
}

#[test]
fn empty_source_does_not_panic() {
    parse_does_not_panic("");
    parse_does_not_panic(" \n\t\n  ");
}

#[test]
fn truncated_program_at_every_boundary_is_safe() {
    // Walk the canonical "fn main() {}" string truncating one
    // character at a time. Each prefix should parse cleanly or
    // produce diagnostics - never panic.
    let full = "fn main() { let x = 1 + 2; println!(\"{}\", x) }";
    for n in 0..=full.len() {
        // Only respect char boundaries - slicing inside a
        // multi-byte char would itself panic in Rust before the
        // parser sees it.
        if !full.is_char_boundary(n) {
            continue;
        }
        parse_does_not_panic(&full[..n]);
    }
}

#[test]
fn deeply_nested_expressions_do_not_overflow_the_parser_stack() {
    // 200-level deep nesting via parentheses. A naive recursive
    // descent without a depth guard would blow the stack here;
    // the parser should either recurse safely or surface a
    // depth-limit diagnostic.
    let mut s = String::from("fn main() { let _ = ");
    s.push_str(&"(".repeat(200));
    s.push('1');
    s.push_str(&")".repeat(200));
    s.push_str("; }");
    parse_does_not_panic(&s);
}

#[test]
fn unbalanced_braces_produce_diagnostics_not_panics() {
    parse_does_not_panic("fn main() {");
    parse_does_not_panic("fn main() {{{ }");
    parse_does_not_panic("}");
    parse_does_not_panic("fn main() { let x = ");
}

#[test]
fn unicode_identifier_attempts_handled_gracefully() {
    // Non-ASCII identifier characters. The parser may reject
    // them, accept them, or report a diagnostic - but it must
    // not panic in the lexer or token scanner.
    parse_does_not_panic("fn main() { let café = 1 }");
    parse_does_not_panic("fn main() { let π = 3.14 }");
    parse_does_not_panic("fn main() { let 日本語 = 0 }");
}

#[test]
fn pathological_string_literals_do_not_panic() {
    // Unterminated, weird escapes, embedded newlines - every
    // shape the lexer might mishandle.
    parse_does_not_panic("fn main() { let s = \"unterm");
    parse_does_not_panic("fn main() { let s = \"\\x99\\u{ffff}\\\" }");
    parse_does_not_panic("fn main() { let s = \"a\nb\" }");
    parse_does_not_panic("fn main() { let s = \"\" }");
}

#[test]
fn random_punctuation_soup_does_not_panic() {
    // Adversarial input shapes - these aren't valid Gossamer
    // but the parser shouldn't crash on them.
    let cases = [
        ";;;;;;;;;",
        "}{}{}{}{}{}",
        ",,,,,,,,,",
        "( ) [ ] { }",
        "fn () -> () { }",
        "let",
        "match { }",
        "...",
        "!@#$%^&*",
    ];
    for case in &cases {
        parse_does_not_panic(case);
    }
}

#[test]
fn extremely_long_identifier_is_handled() {
    // 10 KB identifier. The lexer should accept any length
    // without quadratic-or-worse behaviour.
    let ident: String = "a".repeat(10_000);
    let source = format!("fn main() {{ let {ident} = 1 }}");
    parse_does_not_panic(&source);
}

#[test]
fn very_large_integer_literal_is_handled() {
    // i128::MAX overflows i64 but the parser should still
    // produce a value (or a diagnostic) without panicking.
    parse_does_not_panic("fn main() { let n = 99999999999999999999999999999 }");
    parse_does_not_panic("fn main() { let n = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF }");
}

#[test]
fn unterminated_raw_string_with_multibyte_tail_does_not_panic() {
    // Regression for an mir_lower-fuzz panic: an unterminated raw
    // string `r"...ڍ` left the suffix offset inside the multi-byte
    // `ڍ` (U+068D, bytes 4..6) and `extract_raw_string_body` sliced
    // mid-codepoint, panicking the parser.
    parse_does_not_panic("fn a() { let s = r\"\0\0ڍ");
    parse_does_not_panic("fn a() { let s = r#\"\0\0ڍ");
    parse_does_not_panic("fn a() { let s = br\"\0\0ڍ");
    parse_does_not_panic("fn a() { let s = r\"ڍ\"; }");
}

#[test]
fn item_recovery_makes_forward_progress_on_item_start_keywords() {
    // Regression for the fuzz OOM where an item-start keyword
    // (e.g. `use`) appearing where no item parser handled it
    // trapped `recover_to_item_start` (already at item-start) in
    // a no-op. The outer loop's progress check then re-invoked
    // recovery on the same token forever, pushing one stub Item
    // per iteration until RSS blew past 600 MB.
    parse_does_not_panic("fn a() {}\nuse\n");
    parse_does_not_panic("fn a() {}\nuse let ing()");
    parse_does_not_panic("fn a() {}\nfn\nfn\nfn");
    parse_does_not_panic("struct S {}\nimpl\nimpl\nimpl");
    parse_does_not_panic("\0use");
    parse_does_not_panic("fn a(){} pub pub pub pub pub");
    parse_does_not_panic("fn maenum E enum E {\n{\n    A,\n    B    A,\n   in() {\n");
    // The exact 173-byte input from the CI fuzz crash.
    parse_does_not_panic(concat!(
        "pub fn greet(name: &str) ->  2ng() + nam -> String {64 { a * b }\n",
        "use let ing() + nameString {64 {Ha * b }\n",
        "    let 2ng() + nam -> String {64 { a * b }\n",
        "use let ing() + name\n}\n",
    ));
}
