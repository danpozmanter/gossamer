//! Integration tests for the Gossamer lexer.
//! Each test drives `tokenize` on a small source snippet and asserts
//! on the resulting token kinds, span coverage, or diagnostics.

use gossamer_lex::{FileId, Keyword, LexError, Punct, SourceMap, Token, TokenKind, tokenize};

/// Returns a fresh `FileId` from a throwaway `SourceMap`, because the
/// lexer expects spans to reference a real file.
fn test_file() -> FileId {
    let mut map = SourceMap::new();
    map.add_file("test.gos", "")
}

/// Convenience: returns every kind of a tokenised stream, skipping
/// whitespace and trailing `Eof`.
fn kinds_of(source: &str) -> Vec<TokenKind> {
    let (tokens, _) = tokenize(source, test_file());
    tokens
        .into_iter()
        .filter(|token| !matches!(token.kind, TokenKind::Whitespace | TokenKind::Eof))
        .map(|token: Token| token.kind)
        .collect()
}

/// Lexing an empty source yields exactly one `Eof` token.
#[test]
fn empty_source_yields_eof() {
    let (tokens, diagnostics) = tokenize("", test_file());
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].kind, TokenKind::Eof);
    assert!(diagnostics.is_empty());
}

/// Every lexical keyword round-trips through `Keyword::from_ident`.
#[test]
fn every_keyword_is_recognized() {
    let keywords = [
        "as", "async", "await", "break", "const", "continue", "crate", "defer", "else", "enum",
        "extern", "false", "fn", "for", "go", "if", "impl", "in", "let", "loop", "match", "mod",
        "mut", "package", "pub", "return", "select", "self", "Self", "static", "struct", "super",
        "trait", "true", "type", "unsafe", "use", "where", "while", "yield",
    ];
    for word in keywords {
        assert!(
            Keyword::from_ident(word).is_some(),
            "keyword {word} should be recognized",
        );
    }
}

/// `import` is not a Gossamer keyword — it lexes as a plain identifier.
#[test]
fn import_is_a_plain_identifier() {
    assert!(Keyword::from_ident("import").is_none());
    assert_eq!(kinds_of("import"), vec![TokenKind::Ident]);
}

/// Simple `use` declaration tokenises as keyword + identifier.
#[test]
fn use_declaration_tokenises() {
    let kinds = kinds_of("use fmt");
    assert_eq!(
        kinds,
        vec![TokenKind::Keyword(Keyword::Use), TokenKind::Ident],
    );
}

/// The pipe operator `|>` beats bare `|` under longest-match.
#[test]
fn pipe_gt_is_longest_match() {
    let kinds = kinds_of("|| |> |= |");
    assert_eq!(
        kinds,
        vec![
            TokenKind::Punct(Punct::PipePipe),
            TokenKind::Punct(Punct::PipeGt),
            TokenKind::Punct(Punct::PipeEq),
            TokenKind::Punct(Punct::Pipe),
        ],
    );
}

/// Three-character shift-assign operators win against two-character prefixes.
#[test]
fn three_char_operators_win() {
    let kinds = kinds_of("<<= >>= ..= ...");
    assert_eq!(
        kinds,
        vec![
            TokenKind::Punct(Punct::ShiftLEq),
            TokenKind::Punct(Punct::ShiftREq),
            TokenKind::Punct(Punct::DotDotEq),
            TokenKind::Punct(Punct::DotDotDot),
        ],
    );
}

/// Integer literals with base prefixes and suffixes.
#[test]
fn integer_literals_with_prefixes() {
    let kinds = kinds_of("0 42 0b1010 0o755 0xDEAD_BEEF 100_000u64");
    assert_eq!(kinds, vec![TokenKind::IntLit; 6]);
}

/// Float literals with fractional and exponent parts.
#[test]
fn float_literals_parse() {
    let kinds = kinds_of("3.14 6.022e23 1.0f32 2E-5");
    assert_eq!(kinds, vec![TokenKind::FloatLit; 4]);
}

/// A decimal integer followed by `.` and an identifier is a method
/// call, not a float.
#[test]
fn integer_then_dot_is_not_a_float() {
    let kinds = kinds_of("42.foo");
    assert_eq!(
        kinds,
        vec![
            TokenKind::IntLit,
            TokenKind::Punct(Punct::Dot),
            TokenKind::Ident,
        ],
    );
}

/// String literals handle simple escapes without diagnostics.
#[test]
fn string_literal_with_escapes() {
    let (tokens, diagnostics) = tokenize(r#""hello\n\"world\"""#, test_file());
    let kinds: Vec<TokenKind> = tokens
        .iter()
        .filter(|token| !matches!(token.kind, TokenKind::Whitespace | TokenKind::Eof))
        .map(|token| token.kind)
        .collect();
    assert_eq!(kinds, vec![TokenKind::StringLit]);
    assert!(diagnostics.is_empty());
}

/// Raw strings with `#` padding support embedded `"`.
#[test]
fn raw_string_with_hashes() {
    let source = "r#\"has a \" quote\"#";
    let kinds = kinds_of(source);
    assert_eq!(kinds, vec![TokenKind::RawStringLit { hashes: 1 }]);
}

/// An unterminated string literal produces a diagnostic.
#[test]
fn unterminated_string_reports_diagnostic() {
    let (_, diagnostics) = tokenize("\"oh no", test_file());
    assert!(matches!(
        diagnostics.as_slice(),
        [LexError::UnterminatedString { .. }],
    ));
}

/// Line comments consume to (but not including) the next newline.
#[test]
fn line_comment_stops_at_newline() {
    let kinds = kinds_of("// a comment\nfn main() {}");
    assert_eq!(
        kinds,
        vec![
            TokenKind::LineComment,
            TokenKind::Keyword(Keyword::Fn),
            TokenKind::Ident,
            TokenKind::Punct(Punct::LParen),
            TokenKind::Punct(Punct::RParen),
            TokenKind::Punct(Punct::LBrace),
            TokenKind::Punct(Punct::RBrace),
        ],
    );
}

/// Block comments require a closing `*/`; the unterminated case is
/// reported but the lexer still makes progress.
#[test]
fn unterminated_block_comment_reports_diagnostic() {
    let (tokens, diagnostics) = tokenize("/* oh no", test_file());
    assert!(
        tokens
            .iter()
            .any(|token| token.kind == TokenKind::BlockComment),
        "expected at least one block-comment token",
    );
    assert!(matches!(
        diagnostics.as_slice(),
        [LexError::UnterminatedBlockComment { .. }],
    ));
}

/// Block comments nest: `/* outer /* inner */ still-outer */` is a
/// single comment. Closing only the innermost `*/` must not
/// terminate the outer.
#[test]
fn block_comments_nest() {
    let source = "/* outer /* inner */ still-outer */ fn";
    let (tokens, diagnostics) = tokenize(source, test_file());
    assert!(
        diagnostics.is_empty(),
        "nested block comment should not diagnose: {diagnostics:?}"
    );
    let block_count = tokens
        .iter()
        .filter(|t| t.kind == TokenKind::BlockComment)
        .count();
    assert_eq!(
        block_count, 1,
        "expected a single BlockComment token, got {block_count}"
    );
    let has_fn_keyword = tokens
        .iter()
        .any(|t| matches!(t.kind, TokenKind::Keyword(gossamer_lex::Keyword::Fn)));
    assert!(has_fn_keyword, "`fn` after the comment should still lex");
}

/// An unbalanced nested block comment (more `/*` than `*/`)
/// diagnoses as unterminated.
#[test]
fn unbalanced_nested_block_comment_is_unterminated() {
    let (_tokens, diagnostics) = tokenize("/* outer /* inner still open", test_file());
    assert!(matches!(
        diagnostics.as_slice(),
        [LexError::UnterminatedBlockComment { .. }],
    ));
}

/// Every token's span covers a contiguous, non-overlapping slice of
/// the original source: the spans tile the input exactly.
#[test]
fn spans_tile_source_without_gaps() {
    let source = "use fmt\nfn main() { fmt::println(\"hi\") }\n";
    let (tokens, diagnostics) = tokenize(source, test_file());
    assert!(diagnostics.is_empty());
    let mut cursor: u32 = 0;
    for token in &tokens {
        assert_eq!(token.span.start, cursor, "gap at {cursor}");
        cursor = token.span.end;
    }
    assert_eq!(cursor as usize, source.len());
}

/// A byte literal `b'x'` lexes as a single `ByteLit` token.
#[test]
fn byte_literal_is_recognized() {
    let kinds = kinds_of("b'x'");
    assert_eq!(kinds, vec![TokenKind::ByteLit]);
}

/// The `br"..."` prefix lexes as a raw byte string.
#[test]
fn raw_byte_string_is_recognized() {
    let kinds = kinds_of("br\"abc\"");
    assert_eq!(kinds, vec![TokenKind::RawByteStringLit { hashes: 0 }]);
}

/// An invalid byte (here a backtick) emits `Invalid` and a diagnostic
/// but the lexer still completes.
#[test]
fn stray_backtick_is_invalid() {
    let (tokens, diagnostics) = tokenize("`", test_file());
    assert_eq!(tokens[0].kind, TokenKind::Invalid);
    assert!(matches!(
        diagnostics.as_slice(),
        [LexError::UnexpectedChar { .. }],
    ));
}

/// The full pipe chain from SUMMARY.md tokenises without diagnostics.
#[test]
fn summary_pipe_chain_tokenises() {
    let source = "1..=100 |> iter::filter(|n| n % 2 == 0) |> iter::map(|n| n * n)";
    let (_, diagnostics) = tokenize(source, test_file());
    assert!(
        diagnostics.is_empty(),
        "unexpected diagnostics: {diagnostics:?}"
    );
}

/// The Gossamer examples tokenise without diagnostics.
#[test]
fn hello_world_tokenises_cleanly() {
    let source = "use fmt\n\nfn main() {\n    fmt::println(\"hello, world\")\n}\n";
    let (_, diagnostics) = tokenize(source, test_file());
    assert!(
        diagnostics.is_empty(),
        "unexpected diagnostics: {diagnostics:?}"
    );
}
