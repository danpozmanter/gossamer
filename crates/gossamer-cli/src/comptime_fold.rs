//! Comptime fold pass.
//!
//! `comptime { ... }` blocks and `comptime fn` calls are evaluated on
//! the bytecode VM during compilation and spliced back into the source
//! as literals. Running once here, ahead of the per-tier pipelines,
//! guarantees the bytecode VM, the Cranelift JIT, and the LLVM AOT
//! backend all compile the identical constant - comptime never reaches
//! a backend.
//!
//! The pass loads a throwaway VM over the already-augmented source,
//! evaluates each comptime region, and replaces its source span with
//! the result literal. Programs with no `comptime` spelling skip the
//! whole pass.

use anyhow::{Result, anyhow};
use gossamer_interp::value::Value;

/// Evaluates every comptime region in `augmented` (autoderive-augmented
/// source) and returns the source with each region replaced by its
/// result literal. Returns `augmented` unchanged when it contains no
/// `comptime` spelling, or when the front-end gate rejects the program
/// (the caller's real pass re-runs the gate and reports those errors).
/// Returns `Err` when a comptime region is not compile-time-known or
/// does not evaluate to a scalar or string.
pub(crate) fn fold_comptime(augmented: &str, file_label: &str) -> Result<String> {
    if !augmented.contains("comptime") {
        return Ok(augmented.to_string());
    }

    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file_label.to_string(), augmented.to_string());
    let outcome = gossamer_driver::check_frontend(augmented, file_id);
    if !outcome.is_ok() {
        // Other front-end errors exist; let the caller's authoritative
        // gate render them rather than masking them behind a comptime
        // failure.
        return Ok(augmented.to_string());
    }

    let gossamer_driver::CheckedFrontend {
        sf,
        resolutions,
        table,
        mut tcx,
    } = outcome.checked;
    let program = gossamer_hir::lower_source_file(&sf, &resolutions, &table, &mut tcx);

    let mut vm = gossamer_interp::Vm::new();
    vm.set_collect_comptime(true);
    vm.load(&program, tcx, false)
        .map_err(|err| anyhow!("comptime evaluation failed: {err}"))?;
    let folds = vm.take_comptime_folds();
    drop(program);

    // Apply replacements right-to-left so earlier byte offsets stay
    // valid as later regions are spliced. Outermost regions never
    // overlap, so a stable descending sort by start is sufficient.
    let mut repls: Vec<(usize, usize, String)> = Vec::with_capacity(folds.len());
    for (span, outcome) in folds {
        let start = span.start as usize;
        let end = span.end as usize;
        let literal = match outcome {
            Ok(value) => render_literal(&value).ok_or_else(|| {
                anyhow!(
                    "{}: comptime result must be a scalar or string",
                    locate(augmented, file_label, start)
                )
            })?,
            Err(message) => {
                return Err(anyhow!(
                    "{}: {message}",
                    locate(augmented, file_label, start)
                ));
            }
        };
        repls.push((start, end, literal));
    }
    repls.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));

    let mut folded = augmented.to_string();
    for (start, end, literal) in repls {
        folded.replace_range(start..end, &literal);
    }
    Ok(folded)
}

/// Renders a comptime result value as a Gossamer source literal, or
/// `None` when the value is not a scalar or string (the P0 boundary).
fn render_literal(value: &Value) -> Option<String> {
    Some(match value {
        Value::Unit | Value::Void => "()".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Uint(u) => u.to_string(),
        Value::Float(f) if f.is_finite() => {
            // `{:?}` renders f64 with a decimal point (`64.0`, not
            // `64`) so the spliced literal re-parses as a float and
            // round-trips to the same value.
            format!("{f:?}")
        }
        Value::Char(c) => format!("'{}'", escape_char(*c)),
        Value::String(s) => format!("\"{}\"", escape_string(s.as_str())),
        _ => return None,
    })
}

fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_char(c: char) -> String {
    match c {
        '\\' => "\\\\".to_string(),
        '\'' => "\\'".to_string(),
        '\n' => "\\n".to_string(),
        '\r' => "\\r".to_string(),
        '\t' => "\\t".to_string(),
        _ => c.to_string(),
    }
}

/// Renders `file:line:col` for the byte offset `pos` in `source`.
fn locate(source: &str, file_label: &str, pos: usize) -> String {
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, ch) in source.char_indices() {
        if i >= pos {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    format!("{file_label}:{line}:{col}")
}
