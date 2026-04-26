//! Smoke-test the lexer's single-threaded throughput on a synthetic
//! ~100 KiB source. exit criterion: ≥ 50 MB/s in release mode.
//! The assertion in this test only fires when the crate is built in
//! release mode so debug-mode CI does not flap.

use gossamer_lex::{FileId, SourceMap, TokenKind, tokenize};
use std::time::Instant;

/// Runs the lexer over a large synthetic source and reports the
/// measured throughput.
#[test]
fn throughput_meets_target_in_release() {
    let source = build_source(100 * 1024);
    let mut map = SourceMap::new();
    let file: FileId = map.add_file("bench.gos", source.clone());

    let iterations = 4;
    let start = Instant::now();
    let mut total_tokens: usize = 0;
    for _ in 0..iterations {
        let (tokens, _) = tokenize(&source, file);
        total_tokens += tokens.len();
    }
    let elapsed = start.elapsed();
    let bytes_per_iteration =
        f64::from(u32::try_from(source.len()).expect("synthetic source fits in u32"));
    let total_bytes = bytes_per_iteration * f64::from(iterations);
    let megabytes_per_second = total_bytes / elapsed.as_secs_f64() / 1_048_576.0;

    println!(
        "lexed {total_tokens} tokens across {iterations} runs of {} bytes in {:?} = {megabytes_per_second:.1} MB/s",
        source.len(),
        elapsed,
    );

    if cfg!(not(debug_assertions)) {
        assert!(
            megabytes_per_second >= 50.0,
            "throughput {megabytes_per_second:.1} MB/s below 50 MB/s target",
        );
    }

    assert_ne!(
        tokenize(&source, file).0.last().map(|token| token.kind),
        Some(TokenKind::Invalid),
    );
}

/// Builds a synthetic source roughly `target_bytes` in length, made of
/// representative Gossamer-ish code so the lexer exercises every path.
fn build_source(target_bytes: usize) -> String {
    let snippet = concat!(
        "use fmt\n",
        "use std::sync::atomic::{AtomicU64, Ordering}\n",
        "\n",
        "struct Counter { hits: AtomicU64 }\n",
        "\n",
        "fn bump(counter: &Counter) -> u64 {\n",
        "    counter.hits.fetch_add(1, Ordering::Relaxed) + 1\n",
        "}\n",
        "\n",
        "fn pipeline(values: Vec<i64>) -> i64 {\n",
        "    values |> iter::filter(|n| n % 2 == 0) |> iter::sum::<i64>()\n",
        "}\n",
        "\n",
        "// A short comment to exercise the comment path.\n",
        "/* And a block comment with *stars* inside. */\n",
        "\n",
    );
    let mut source = String::with_capacity(target_bytes + snippet.len());
    while source.len() < target_bytes {
        source.push_str(snippet);
    }
    source
}
