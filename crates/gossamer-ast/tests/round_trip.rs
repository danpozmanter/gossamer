//! Stability tests: build an AST, print twice, assert the outputs match.
//! Until a parser is available, a proxy for round-tripping is printing the
//! same AST twice and asserting byte equality — this confirms the printer is
//! deterministic and that repeated renders produce identical output.

#![forbid(unsafe_code)]

mod common;

use common::{
    binary, block_expr, call_expr, expr_stmt, field_access, fn_item, fn_item_with_ret, let_stmt,
    literal_int, literal_string, method_call_expr, path_expr, source_file, type_path,
    use_decl_module,
};

use gossamer_ast::{BinaryOp, Expr, ExprKind, Literal, NodeId, SourceFile};
use gossamer_lex::{FileId, SourceMap, Span};

fn span() -> Span {
    let mut map = SourceMap::new();
    let file: FileId = map.add_file("round-trip", "");
    Span::new(file, 0, 0)
}

fn print_twice(source: &SourceFile) -> (String, String) {
    let first = source.to_string();
    let second = source.to_string();
    (first, second)
}

#[test]
fn hello_world_render_is_stable_across_repeated_display() {
    let println_call = call_expr(
        path_expr(&["fmt", "println"]),
        vec![literal_string("hello, world")],
    );
    let body = block_expr(vec![expr_stmt(println_call, false)], None);
    let source = source_file(vec![use_decl_module(&["fmt"])], vec![fn_item("main", body)]);

    let (first, second) = print_twice(&source);
    assert_eq!(first, second);
    assert!(first.contains("fn main()"));
}

#[test]
fn web_server_report_stats_loop_is_stable() {
    let sleep_call = call_expr(path_expr(&["time", "sleep"]), vec![literal_int("5000")]);
    let println_call = call_expr(
        path_expr(&["fmt", "println"]),
        vec![
            literal_string("stats:"),
            method_call_expr(
                field_access(path_expr(&["counter"]), "total"),
                "call",
                vec![],
            ),
            literal_string("requests served"),
        ],
    );
    let loop_body = block_expr(
        vec![expr_stmt(sleep_call, false), expr_stmt(println_call, false)],
        None,
    );
    let loop_expr = Expr::new(
        NodeId::DUMMY,
        span(),
        ExprKind::Loop {
            label: None,
            body: Box::new(loop_body),
        },
    );
    let fn_body = block_expr(vec![expr_stmt(loop_expr, false)], None);
    let source = source_file(vec![], vec![fn_item("report_stats", fn_body)]);

    let (first, second) = print_twice(&source);
    assert_eq!(first, second);
    assert!(first.contains("loop {"));
}

#[test]
fn line_count_pipe_chain_is_stable() {
    let base = method_call_expr(path_expr(&["rx"]), "into_iter", vec![]);
    let hop1 = call_expr(path_expr(&["iter", "inspect"]), vec![literal_int("0")]);
    let hop2 = call_expr(path_expr(&["iter", "map"]), vec![literal_int("0")]);
    let hop3 = call_expr(path_expr(&["iter", "sum"]), vec![]);
    let stage1 = binary(BinaryOp::PipeGt, base, hop1);
    let stage2 = binary(BinaryOp::PipeGt, stage1, hop2);
    let chain = binary(BinaryOp::PipeGt, stage2, hop3);

    let let_line = let_stmt("total", chain);
    let body = block_expr(vec![let_line], None);
    let source = source_file(vec![], vec![fn_item("main", body)]);

    let (first, second) = print_twice(&source);
    assert_eq!(first, second);
    assert!(first.contains("|>"));
    let pipe_count = first.matches("|>").count();
    assert_eq!(pipe_count, 3, "expected 3 pipes, got output:\n{first}");
}

#[test]
fn pipe_chain_with_three_hops_emits_one_pipe_per_line() {
    let base = path_expr(&["x"]);
    let first_hop = path_expr(&["a"]);
    let second_hop = path_expr(&["b"]);
    let third_hop = path_expr(&["c"]);
    let stage1 = binary(BinaryOp::PipeGt, base, first_hop);
    let stage2 = binary(BinaryOp::PipeGt, stage1, second_hop);
    let chain = binary(BinaryOp::PipeGt, stage2, third_hop);

    let body = block_expr(vec![let_stmt("total", chain)], None);
    let source = source_file(vec![], vec![fn_item("main", body)]);
    let rendered = source.to_string();

    let pipe_lines = rendered
        .lines()
        .filter(|line| line.trim_start().starts_with("|>"))
        .count();
    assert_eq!(pipe_lines, 3, "rendered:\n{rendered}");
}

#[test]
fn single_hop_pipe_stays_inline() {
    let base = path_expr(&["x"]);
    let hop = path_expr(&["f"]);
    let pipe = binary(BinaryOp::PipeGt, base, hop);
    let body = block_expr(vec![expr_stmt(pipe, false)], None);
    let source = source_file(vec![], vec![fn_item("main", body)]);
    let rendered = source.to_string();

    assert!(rendered.contains("x |> f"), "rendered:\n{rendered}");
    let pipe_lines = rendered
        .lines()
        .filter(|line| line.trim_start().starts_with("|>"))
        .count();
    assert_eq!(pipe_lines, 0, "expected inline pipe, got:\n{rendered}");
}

#[test]
fn multiple_items_separated_by_blank_line() {
    let first_fn = fn_item("first", block_expr(vec![], None));
    let second_fn = fn_item("second", block_expr(vec![], None));
    let source = source_file(vec![], vec![first_fn, second_fn]);
    let rendered = source.to_string();
    assert!(rendered.contains("fn first() {}\n\nfn second() {}\n"));
}

#[test]
fn binary_precedence_parenthesises_only_when_needed() {
    let inner = binary(BinaryOp::Add, literal_int("1"), literal_int("2"));
    let multiplied = binary(BinaryOp::Mul, inner, literal_int("3"));
    let body = block_expr(vec![expr_stmt(multiplied, false)], None);
    let source = source_file(vec![], vec![fn_item("calc", body)]);
    let rendered = source.to_string();
    assert!(rendered.contains("(1 + 2) * 3"), "rendered:\n{rendered}");
}

#[test]
fn fn_with_return_type_renders_arrow_and_type() {
    let return_ty = type_path(&["i32"]);
    let body = block_expr(vec![], Some(literal_int("7")));
    let item = fn_item_with_ret("answer", body, Some(return_ty));
    let rendered = source_file(vec![], vec![item]).to_string();
    assert!(
        rendered.contains("fn answer() -> i32 {"),
        "rendered:\n{rendered}"
    );
    assert!(rendered.contains("    7\n"), "rendered:\n{rendered}");
}

#[test]
fn integer_literal_preserves_original_text() {
    let expr = Expr::new(
        NodeId::DUMMY,
        span(),
        ExprKind::Literal(Literal::Int("0x2a".into())),
    );
    let body = block_expr(vec![expr_stmt(expr, false)], None);
    let rendered = source_file(vec![], vec![fn_item("main", body)]).to_string();
    assert!(rendered.contains("0x2a"), "rendered:\n{rendered}");
}
