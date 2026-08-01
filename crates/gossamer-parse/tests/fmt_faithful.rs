//! Faithfulness tests for the token-stream formatter behind `gos fmt`.
//!
//! Runs `format_source` over the whole `.gos` corpus (`examples/` and
//! `feature-testing-examples/`) and asserts the three core guarantees:
//! token equivalence (no code token altered, merged, or dropped),
//! comment preservation (count and content unchanged), and
//! idempotency (`fmt(fmt(x)) == fmt(x)`), plus targeted regressions
//! for the constructs the old AST printer used to destroy.

use std::path::PathBuf;

use gossamer_lex::{FileId, SourceMap, TokenKind, tokenize};
use gossamer_parse::format_source;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn corpus_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in ["examples", "feature-testing-examples"] {
        let dir = workspace_root().join(dir);
        let entries =
            std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("gos") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn file_id(map: &mut SourceMap, name: &str, source: &str) -> FileId {
    map.add_file(name.to_string(), source.to_string())
}

fn fmt(source: &str) -> String {
    let mut map = SourceMap::new();
    let file = file_id(&mut map, "fixture.gos", source);
    format_source(source, file).expect("format fixture")
}

#[test]
fn removes_optional_line_ending_semicolons() {
    let source = "use std::strings;\nfn main() {\n    let value = 1;\n    println(value);\n}\n";
    let formatted = fmt(source);
    assert_eq!(
        formatted,
        "use std::strings\n\nfn main() {\n    let value = 1\n    println(value)\n}\n"
    );
}

fn significant(source: &str) -> Vec<(TokenKind, String)> {
    let mut map = SourceMap::new();
    let file = file_id(&mut map, "sig.gos", source);
    let (tokens, errs) = tokenize(source, file);
    assert!(errs.is_empty(), "lex errors in input");
    tokens
        .iter()
        .enumerate()
        .filter(|(index, token)| {
            if matches!(token.kind, TokenKind::Whitespace | TokenKind::Eof) {
                return false;
            }
            if token.kind == TokenKind::Punct(gossamer_lex::Punct::Semi) {
                let next = tokens[index + 1..]
                    .iter()
                    .find(|next| !matches!(next.kind, TokenKind::Whitespace));
                let next_start = next.map_or(source.len(), |next| next.span.start as usize);
                if source[token.span.end as usize..next_start].contains('\n')
                    || next.is_none_or(|next| {
                        matches!(
                            next.kind,
                            TokenKind::Punct(gossamer_lex::Punct::RBrace) | TokenKind::Eof
                        )
                    })
                {
                    return false;
                }
            }
            if token.kind != TokenKind::Punct(gossamer_lex::Punct::Comma) {
                return true;
            }
            let next_start = tokens[index + 1..]
                .iter()
                .find(|next| {
                    !matches!(
                        next.kind,
                        TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment
                    )
                })
                .map_or(source.len(), |next| next.span.start as usize);
            !source[token.span.end as usize..next_start].contains('\n')
        })
        .map(|(_, t)| {
            (
                t.kind,
                source[t.span.start as usize..t.span.end as usize].to_string(),
            )
        })
        .collect()
}

fn comments(source: &str) -> Vec<String> {
    significant(source)
        .into_iter()
        .filter(|(kind, _)| matches!(kind, TokenKind::LineComment | TokenKind::BlockComment))
        .map(|(_, text)| text)
        .collect()
}

/// (a) Every corpus file formats successfully and the output's
/// non-whitespace token stream - comments included - is identical to
/// the input's.
#[test]
fn corpus_token_equivalence_after_fmt() {
    let files = corpus_files();
    assert!(
        files.len() > 150,
        "corpus unexpectedly small: {}",
        files.len()
    );
    let mut formatted_count = 0usize;
    for path in &files {
        let source = std::fs::read_to_string(path).expect("read corpus file");
        let mut map = SourceMap::new();
        let file = file_id(&mut map, &path.to_string_lossy(), &source);
        let formatted = format_source(&source, file)
            .unwrap_or_else(|e| panic!("format {}: {e}", path.display()));
        assert_eq!(
            significant(&source),
            significant(&formatted),
            "token stream changed for {}",
            path.display()
        );
        formatted_count += 1;
    }
    assert_eq!(formatted_count, files.len());
}

/// (b) Formatting is idempotent across the whole corpus.
#[test]
fn corpus_fmt_is_idempotent() {
    for path in corpus_files() {
        let source = std::fs::read_to_string(&path).expect("read corpus file");
        let once = fmt(&source);
        let twice = fmt(&once);
        assert_eq!(once, twice, "fmt not idempotent on {}", path.display());
    }
}

/// (c) Comment count and content survive formatting byte-for-byte on
/// every corpus file.
#[test]
fn corpus_comments_preserved() {
    for path in corpus_files() {
        let source = std::fs::read_to_string(&path).expect("read corpus file");
        let formatted = fmt(&source);
        assert_eq!(
            comments(&source),
            comments(&formatted),
            "comments changed in {}",
            path.display()
        );
    }
}

#[test]
fn trailing_comments_stay_trailing() {
    let source =
        "fn main() {\n    let port = 8080  // matches the nginx upstream\n    serve(port)\n}\n";
    assert_eq!(fmt(source), source);
}

#[test]
fn block_comments_survive_inline_and_standalone() {
    let source = "/* header */\nfn main() {\n    let x = 1 /* inline */ + 2\n    /*\n     * multi-line body\n     */\n    use_it(x)\n}\n";
    assert_eq!(fmt(source), source);
}

#[test]
fn comments_inside_match_arms_and_chains() {
    let source = "fn label(n: i64) -> String {\n    match n {\n        // negative side\n        x if x < 0 => \"neg\",\n        // everything else\n        _ => \"pos\",\n    }\n}\n\nfn chain(input: [i64]) -> i64 {\n    input\n        // keep evens only\n        |> filter(|n: i64| n % 2 == 0)\n        |> count\n}\n";
    let expected = "fn label(n: i64) -> String {\n    match n {\n        // negative side\n        x if x < 0 => \"neg\"\n        // everything else\n        _ => \"pos\"\n    }\n}\n\nfn chain(input: [i64]) -> i64 {\n    input\n        // keep evens only\n        |> filter(|n: i64| n % 2 == 0)\n        |> count\n}\n";
    assert_eq!(fmt(source), expected);
}

#[test]
fn match_arm_commas_before_trailing_comments_are_removed() {
    let source =
        "match a {\n    1 => a + 1, // line comment\n    2 => a + 2, /* block comment */\n}\n";
    let expected =
        "match a {\n    1 => a + 1 // line comment\n    2 => a + 2 /* block comment */\n}\n";
    assert_eq!(fmt(source), expected);
}

#[test]
fn println_and_format_macros_never_rewritten() {
    let source = "fn main() {\n    let name = \"jane\"\n    let msg = format!(\"hello, {name}!\")\n    println!(\"{} -> {}\", name, msg)\n    eprintln!(\"warn: {msg}\")\n}\n";
    let out = fmt(source);
    assert_eq!(out, source);
    assert!(!out.contains("__concat"), "macro desugared to __concat");
}

#[test]
fn struct_literals_keep_keyed_forms() {
    let source = "struct Account { owner: String, balance: i64 }\n\nfn main() {\n    let keyed = Account { owner: \"jane\", balance: 1200 }\n}\n";
    assert_eq!(fmt(source), source);
}

#[test]
fn pipe_chains_keep_authored_breaks_and_indent() {
    let source = "fn main() {\n    let words = \"  Hello  World  \"\n        |> split_words\n        |> filter(|w: String| w.len() > 0)\n        |> count\n    println!(\"words: {words}\")\n}\n";
    assert_eq!(fmt(source), source);
}

/// The historical mangling case: a hand-written forwarder with a port
/// number in a comment. The old AST printer deleted every comment and
/// rewrote `println!` into `__concat`; the faithful formatter must
/// return this file byte-identical.
#[test]
fn port_forwarder_fixture_round_trips_byte_identical() {
    let source = "use std::net\n\n// Local URL forwarder.\n// Listens on 127.0.0.1:8080 and forwards to the upstream below.\nconst UPSTREAM: String = \"127.0.0.1:9090\"  // staging box\n\nfn main() {\n    let listener = net::TcpListener::bind(\"127.0.0.1:8080\")\n    // accept loop: one goroutine per connection\n    loop {\n        let stream = listener.accept()\n        go forward(stream)\n    }\n}\n\nfn forward(stream: net::TcpStream) {\n    /* copy bytes both ways until either side closes */\n    let upstream = net::TcpStream::connect(&UPSTREAM)\n    println!(\"forwarding to {}\", UPSTREAM)\n}\n";
    assert_eq!(fmt(source), source);
    assert_eq!(comments(source).len(), 5);
}

#[test]
fn whitespace_normalization_still_happens() {
    assert_eq!(
        fmt("fn    main(  )   {   let x=1+2\n}\n"),
        "fn main() { let x = 1 + 2\n}\n"
    );
}

#[test]
fn check_failures_do_not_modify_anything() {
    let mut map = SourceMap::new();
    let source = "fn broken( {\n";
    let file = map.add_file("broken.gos".to_string(), source.to_string());
    assert!(format_source(source, file).is_err());
}
