//! Interactive REPL.
//!
//! Kept in its own module so `main.rs` stays under the 2000-line
//! hard limit defined in `GUIDELINES.md`.

use anyhow::{Result, anyhow};

use crate::paths::repl_history_path;

#[allow(
    clippy::too_many_lines,
    reason = "REPL loop bundles input, completion, history, and graceful-exit handling"
)]
pub(crate) fn cmd_repl() -> Result<()> {
    use rustyline::error::ReadlineError;
    use rustyline::history::FileHistory;
    use rustyline::{ColorMode, Config, EditMode, Editor};

    use crate::repl_helper::GosReplHelper;

    println!(
        "gos repl - type an expression or declaration\n\
         up/down cycles history · Enter continues until braces close · Ctrl-D or %quit exits"
    );

    let mut transcript: Vec<String> = Vec::new();
    let mut declarations: Vec<String> = Vec::new();
    let mut lets: Vec<String> = Vec::new();
    let mut input_no = 1u32;

    let config = Config::builder()
        .edit_mode(EditMode::Emacs)
        .color_mode(ColorMode::Enabled)
        .auto_add_history(false)
        .build();
    let mut editor: Editor<GosReplHelper, FileHistory> =
        Editor::with_config(config).map_err(|e| anyhow!("repl init: {e}"))?;
    editor.set_helper(Some(GosReplHelper::new()));
    let history_path = repl_history_path();
    if let Some(path) = &history_path {
        let _ = editor.load_history(path);
    }

    let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
    if tty {
        crate::style::force_enable();
    }
    // Greeting on a TTY only - keeps non-interactive consumers
    // (`echo expr | gos`) clean.
    if tty {
        println!(
            "\x1b[1mgos {ver}\x1b[0m  type expressions, or \x1b[36m%help\x1b[0m for meta commands",
            ver = env!("CARGO_PKG_VERSION"),
        );
    }
    loop {
        let prompt = if tty {
            format!("\x1b[32mIn [{input_no}]:\x1b[0m ")
        } else {
            format!("In [{input_no}]: ")
        };
        let line = match editor.readline(&prompt) {
            Ok(line) => line,
            Err(ReadlineError::Eof | ReadlineError::Interrupted) => {
                if let Some(path) = &history_path {
                    let _ = editor.save_history(path);
                }
                println!();
                return Ok(());
            }
            Err(err) => {
                eprintln!("{}: {err}", crate::style::error("repl"));
                return Ok(());
            }
        };
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }
        let _ = editor.add_history_entry(trimmed);
        transcript.push(trimmed.to_string());

        // Meta-commands first.
        if let Some(rest) = trimmed.strip_prefix('%') {
            let rest = rest.trim();
            match rest {
                "quit" | "exit" => {
                    if let Some(path) = &history_path {
                        let _ = editor.save_history(path);
                    }
                    return Ok(());
                }
                "history" => {
                    for (i, entry) in transcript.iter().enumerate() {
                        println!("  {}: {entry}", i + 1);
                    }
                    continue;
                }
                "bindings" => {
                    if lets.is_empty() {
                        println!("    no `let` bindings yet");
                    } else {
                        for (i, b) in lets.iter().enumerate() {
                            println!("  {}: {b}", i + 1);
                        }
                    }
                    continue;
                }
                "reset" => {
                    declarations.clear();
                    lets.clear();
                    println!("session cleared");
                    continue;
                }
                "help" => {
                    println!(
                        "meta-commands: %quit  %history  %bindings  %reset  %help\n\
                         plain expressions render as Out[N]; declarations and\n\
                         `let` bindings persist across inputs."
                    );
                    continue;
                }
                other => {
                    eprintln!("unknown meta-command: %{other}");
                    continue;
                }
            }
        }

        let is_declaration = trimmed.starts_with("fn ")
            || trimmed.starts_with("struct ")
            || trimmed.starts_with("enum ")
            || trimmed.starts_with("use ")
            || trimmed.starts_with("const ")
            || trimmed.starts_with("type ");

        if is_declaration {
            declarations.push(trimmed.to_string());
            match rebuild_session(&declarations) {
                Ok(()) => {
                    println!("    added {} declarations", declarations.len());
                }
                Err(msg) => {
                    declarations.pop();
                    eprintln!("    {msg}");
                }
            }
            input_no += 1;
            continue;
        }

        if trimmed.starts_with("let ") {
            let candidate = trimmed.to_string();
            lets.push(candidate);
            let probe_body = format!("{}\n    ()\n", lets.join("\n    "));
            let probe = format!(
                "{}\nfn __irepl_{n}() {{\n    {body}}}\n",
                declarations.join("\n"),
                n = input_no,
                body = probe_body,
            );
            match build_and_call(&probe, &format!("__irepl_{input_no}")) {
                Ok(_) => {
                    println!("    binding added ({} total)", lets.len());
                }
                Err(msg) => {
                    lets.pop();
                    eprintln!("    {msg}");
                }
            }
            input_no += 1;
            continue;
        }

        let let_body = if lets.is_empty() {
            String::new()
        } else {
            format!("{}\n    ", lets.join("\n    "))
        };
        let program_source = format!(
            "{}\nfn __irepl_{n}() {{ {lets}{expr} }}\n",
            declarations.join("\n"),
            n = input_no,
            lets = let_body,
            expr = trimmed,
        );
        match build_and_call(&program_source, &format!("__irepl_{input_no}")) {
            Ok(value) => {
                if !matches!(value, gossamer_interp::Value::Unit) {
                    if tty {
                        println!("\x1b[31mOut[{input_no}]:\x1b[0m {value}");
                    } else {
                        println!("Out[{input_no}]: {value}");
                    }
                }
            }
            Err(msg) => {
                eprintln!("{}: {msg}", crate::style::error("error"));
            }
        }
        input_no += 1;
    }
}

/// Validates that the accumulated declarations parse, resolve, and
/// compile onto the VM. The built `Vm` is discarded - the REPL keeps
/// declarations as source strings and full-recompiles each input - so
/// this is purely a probe: `Ok(())` means the declaration set is
/// loadable, `Err` rolls back the just-added declaration.
fn rebuild_session(declarations: &[String]) -> std::result::Result<(), String> {
    let source = declarations.join("\n") + "\nfn __irepl_probe() { }\n";
    let mut map = gossamer_lex::SourceMap::new();
    let file = map.add_file("irepl".to_string(), source.clone());
    let (sf, parse_diags) = gossamer_parse::parse_source_file(&source, file);
    if !parse_diags.is_empty() {
        return Err(format_parse_diags(&parse_diags, &map, file));
    }
    let (res, _) = gossamer_resolve::resolve_source_file(&sf);
    let mut tcx = gossamer_types::TyCtxt::new();
    let (tbl, _) = gossamer_types::typecheck_source_file(&sf, &res, &mut tcx);
    let program = gossamer_hir::lower_source_file(&sf, &res, &tbl, &mut tcx);
    let mut vm = gossamer_interp::Vm::new();
    vm.load(&program, tcx, true).map_err(|e| format!("{e}"))?;
    Ok(())
}

fn build_and_call(
    source: &str,
    entry: &str,
) -> std::result::Result<gossamer_interp::Value, String> {
    let mut map = gossamer_lex::SourceMap::new();
    let file = map.add_file("irepl".to_string(), source.to_string());
    let (sf, parse_diags) = gossamer_parse::parse_source_file(source, file);
    if !parse_diags.is_empty() {
        return Err(format_parse_diags(&parse_diags, &map, file));
    }
    let (res, _) = gossamer_resolve::resolve_source_file(&sf);
    let mut tcx = gossamer_types::TyCtxt::new();
    let (tbl, _) = gossamer_types::typecheck_source_file(&sf, &res, &mut tcx);
    let program = gossamer_hir::lower_source_file(&sf, &res, &tbl, &mut tcx);
    let mut vm = gossamer_interp::Vm::new();
    vm.load(&program, tcx, true).map_err(|e| format!("{e}"))?;
    vm.call(entry, Vec::new()).map_err(|e| format!("{e}"))
}

/// Renders a parse-diagnostic batch as one human-readable line per
/// error, prefixed by the count, so REPL users see *what* went wrong
/// instead of just "N parse error(s)". Each entry is annotated with
/// the one-based line / column derived from the source map.
fn format_parse_diags(
    diags: &[gossamer_parse::ParseDiagnostic],
    map: &gossamer_lex::SourceMap,
    file: gossamer_lex::FileId,
) -> String {
    let mut out = if diags.len() == 1 {
        String::from("1 parse error:\n")
    } else {
        format!("{} parse errors:\n", diags.len())
    };
    for diag in diags {
        let pos = map.line_col(file, diag.span.start);
        out.push_str(&format!("  {}:{}: {}\n", pos.line, pos.column, diag.error));
    }
    // Trim trailing newline so the surrounding `eprintln!` doesn't
    // double-space.
    if out.ends_with('\n') {
        out.pop();
    }
    out
}
