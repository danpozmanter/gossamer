//! Grammar-aware Gossamer-source generator for fuzz targets
//!
//!
//! Raw `&[u8]` inputs spend most fuzz cycles failing UTF-8
//! validation or stopping at the first stray byte. Grammar-driven
//! generation feeds the fuzzer well-shaped programs that exercise
//! interesting parser / typechecker / lowerer paths. We still
//! accept arbitrary bytes (libFuzzer's mutator is byte-level),
//! but the bytes are interpreted as a tape of choices for a
//! finite grammar walk - so a single bit-flip biases toward
//! "produce a different operator" instead of "produce invalid
//! UTF-8."

use arbitrary::{Arbitrary, Unstructured};

/// Maximum source size produced. Keeps individual fuzz iterations
/// bounded; larger inputs are clipped.
pub const MAX_SOURCE_BYTES: usize = 4096;

/// Renders an `Arbitrary` choice tape into a Gossamer source
/// fragment. Always emits a syntactically-plausible body - even
/// degenerate tapes produce valid AST shapes.
///
/// `pub` (not `pub(crate)`) because the fuzz target bins are
/// separate cargo crates that depend on this lib; `pub(crate)`
/// would hide it from them and the build fails with
/// `function `render_source` is private`.
pub fn render_source(seed: &[u8]) -> String {
    let mut u = Unstructured::new(seed);
    let prog = Program::arbitrary(&mut u).unwrap_or_default();
    let mut out = String::new();
    prog.emit(&mut out, 0);
    if out.len() > MAX_SOURCE_BYTES {
        out.truncate(MAX_SOURCE_BYTES);
    }
    out
}

#[derive(Arbitrary, Default)]
struct Program {
    /// Up to 4 top-level functions; the first is `main`.
    fns: Vec<Func>,
}

#[derive(Arbitrary)]
struct Func {
    name_seed: u8,
    body: Stmts,
}

#[derive(Arbitrary, Default)]
struct Stmts(Vec<Stmt>);

#[derive(Arbitrary)]
enum Stmt {
    LetInt(u8, i64),
    LetStr(u8, ShortStr),
    Println(Expr),
    IfPrint(Expr, Expr),
}

#[derive(Arbitrary)]
struct ShortStr(u8);

#[derive(Arbitrary)]
enum Expr {
    Int(i64),
    Var(u8),
    Bin(Box<Expr>, BinOp, Box<Expr>),
    Cmp(Box<Expr>, CmpOp, Box<Expr>),
}

#[derive(Arbitrary)]
enum BinOp { Add, Sub, Mul }

#[derive(Arbitrary)]
enum CmpOp { Eq, Ne, Lt, Gt }

impl Program {
    fn emit(&self, out: &mut String, _depth: u32) {
        let fns: Vec<_> = self.fns.iter().take(4).collect();
        if fns.is_empty() {
            out.push_str("fn main() {}\n");
            return;
        }
        for (i, f) in fns.iter().enumerate() {
            let name = if i == 0 {
                "main".to_string()
            } else {
                format!("f{}", f.name_seed % 16)
            };
            out.push_str(&format!("fn {name}() {{\n"));
            f.body.emit(out);
            out.push_str("}\n");
        }
    }
}

impl Stmts {
    fn emit(&self, out: &mut String) {
        for stmt in self.0.iter().take(8) {
            stmt.emit(out);
        }
    }
}

impl Stmt {
    fn emit(&self, out: &mut String) {
        match self {
            Stmt::LetInt(name, val) => {
                out.push_str(&format!("    let v{}: i64 = {}\n", name % 8, *val));
            }
            Stmt::LetStr(name, s) => {
                out.push_str(&format!("    let s{} = \"{}\"\n", name % 8, s.render()));
            }
            Stmt::Println(e) => {
                out.push_str("    println!(\"{}\", ");
                e.emit(out, 0);
                out.push_str(")\n");
            }
            Stmt::IfPrint(c, e) => {
                out.push_str("    if ");
                c.emit(out, 0);
                out.push_str(" { println!(\"{}\", ");
                e.emit(out, 0);
                out.push_str(") }\n");
            }
        }
    }
}

impl ShortStr {
    fn render(&self) -> String {
        // Render an ASCII-only short string; avoid escape edge
        // cases - the parse fuzz target's seed corpus covers those.
        (0..(self.0 % 8))
            .map(|i| char::from(b'a' + (i % 26)))
            .collect()
    }
}

impl Expr {
    fn emit(&self, out: &mut String, depth: u32) {
        if depth > 4 {
            out.push('0');
            return;
        }
        match self {
            Expr::Int(n) => out.push_str(&format!("{n}")),
            Expr::Var(n) => out.push_str(&format!("v{}", n % 8)),
            Expr::Bin(l, op, r) => {
                out.push('(');
                l.emit(out, depth + 1);
                out.push_str(match op {
                    BinOp::Add => " + ",
                    BinOp::Sub => " - ",
                    BinOp::Mul => " * ",
                });
                r.emit(out, depth + 1);
                out.push(')');
            }
            Expr::Cmp(l, op, r) => {
                out.push('(');
                l.emit(out, depth + 1);
                out.push_str(match op {
                    CmpOp::Eq => " == ",
                    CmpOp::Ne => " != ",
                    CmpOp::Lt => " < ",
                    CmpOp::Gt => " > ",
                });
                r.emit(out, depth + 1);
                out.push(')');
            }
        }
    }
}
