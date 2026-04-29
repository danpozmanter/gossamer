//! Integration tests exercising the immutable and mutable visitor traits.

#![forbid(unsafe_code)]

mod common;

use common::{
    binary, block_expr, call_expr, expr_stmt, field_access, fn_item, let_stmt, literal_int,
    literal_string, method_call_expr, path_expr, source_file,
};

use gossamer_ast::{
    BinaryOp, Expr, ExprKind, Literal, PathExpr, Pattern, SourceFile, Visitor, VisitorMut,
    visitor::{walk_expr, walk_expr_mut, walk_pattern},
};

struct ExpressionCounter {
    count: u32,
}

impl Visitor for ExpressionCounter {
    fn visit_expr(&mut self, expr: &Expr) {
        self.count += 1;
        walk_expr(self, expr);
    }
}

struct LiteralCollector {
    values: Vec<String>,
}

impl Visitor for LiteralCollector {
    fn visit_literal(&mut self, literal: &Literal) {
        match literal {
            Literal::Int(raw) | Literal::Float(raw) => self.values.push(raw.clone()),
            Literal::String(value) => self.values.push(value.clone()),
            _ => {}
        }
    }
}

struct IntDoubler;

impl VisitorMut for IntDoubler {
    fn visit_expr(&mut self, expr: &mut Expr) {
        if let ExprKind::Literal(Literal::Int(raw)) = &mut expr.kind
            && let Ok(value) = raw.parse::<i64>()
        {
            *raw = (value * 2).to_string();
        }
        walk_expr_mut(self, expr);
    }
}

struct PatternNameCollector {
    names: Vec<String>,
}

impl Visitor for PatternNameCollector {
    fn visit_pattern(&mut self, pattern: &Pattern) {
        if let gossamer_ast::PatternKind::Ident { name, .. } = &pattern.kind {
            self.names.push(name.name.clone());
        }
        walk_pattern(self, pattern);
    }
}

fn sample_program() -> SourceFile {
    let first = call_expr(path_expr(&["fmt", "println"]), vec![literal_string("hi")]);
    let second = call_expr(path_expr(&["fmt", "println"]), vec![literal_int("7")]);
    let sum = binary(BinaryOp::Add, literal_int("1"), literal_int("2"));
    let tail = method_call_expr(
        field_access(path_expr(&["self"]), "counter"),
        "bump",
        vec![],
    );
    let body = block_expr(
        vec![
            expr_stmt(first, false),
            expr_stmt(second, false),
            let_stmt("x", sum),
        ],
        Some(tail),
    );
    source_file(vec![], vec![fn_item("main", body)])
}

#[test]
fn expression_visitor_counts_every_nested_expression() {
    let source = sample_program();
    let mut counter = ExpressionCounter { count: 0 };
    counter.visit_source_file(&source);
    assert!(counter.count >= 10, "count was {}", counter.count);
}

#[test]
fn literal_collector_finds_every_string_and_number() {
    let source = sample_program();
    let mut collector = LiteralCollector { values: Vec::new() };
    collector.visit_source_file(&source);
    assert!(collector.values.contains(&"hi".to_string()));
    assert!(collector.values.contains(&"7".to_string()));
    assert!(collector.values.contains(&"1".to_string()));
    assert!(collector.values.contains(&"2".to_string()));
}

#[test]
fn mutator_doubles_all_integer_literals_in_place() {
    let mut source = sample_program();
    IntDoubler.visit_source_file(&mut source);
    let mut collector = LiteralCollector { values: Vec::new() };
    collector.visit_source_file(&source);
    assert!(collector.values.contains(&"14".to_string()));
    assert!(collector.values.contains(&"2".to_string()));
    assert!(collector.values.contains(&"4".to_string()));
}

#[test]
fn pattern_name_collector_captures_let_binding_names() {
    let source = sample_program();
    let mut collector = PatternNameCollector { names: Vec::new() };
    collector.visit_source_file(&source);
    assert!(collector.names.contains(&"x".to_string()));
}

#[test]
fn path_segment_visits_walk_every_segment() {
    struct SegmentCollector {
        all: Vec<String>,
    }
    impl Visitor for SegmentCollector {
        fn visit_path_expr(&mut self, path: &PathExpr) {
            for segment in &path.segments {
                self.all.push(segment.name.name.clone());
            }
        }
    }

    let source = sample_program();
    let mut collector = SegmentCollector { all: Vec::new() };
    collector.visit_source_file(&source);
    assert!(collector.all.contains(&"fmt".to_string()));
    assert!(collector.all.contains(&"println".to_string()));
}

#[test]
fn label_visit_default_is_noop_but_walk_recurses() {
    struct IdentCollector {
        names: Vec<String>,
    }
    impl Visitor for IdentCollector {
        fn visit_path_expr(&mut self, path: &PathExpr) {
            for segment in &path.segments {
                self.names.push(segment.name.name.clone());
            }
        }
    }

    let source = sample_program();
    let mut collector = IdentCollector { names: Vec::new() };
    collector.visit_source_file(&source);
    assert!(!collector.names.is_empty());
}
