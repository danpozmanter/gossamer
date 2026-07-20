//! Interactive REPL.
//!
//! Kept in its own module so `main.rs` stays under the 2000-line
//! hard limit defined in `GUIDELINES.md`.

use anyhow::{Result, anyhow};
use gossamer_parse::builtin_macros::{BUILTIN_MACROS, BuiltinMacro};
use gossamer_std::registry::{StdItem, StdItemKind, StdModule};
use regex::Regex;

use crate::paths::repl_history_path;

const REPL_HELP_TEXT: &str = "meta-commands: %quit (%q)  %history\n\
                         %bindings (%b) [regex]  %declarations (%d) [regex]\n\
                         %reset (%r)  %help (%h)  %ls (%l)  %find (%f) <regex>\n\
                         plain expressions render as Out[N]; declarations and\n\
                         `let` bindings persist across inputs.";

struct PreludeBuiltinHelp {
    name: &'static str,
    signature: &'static str,
    doc: &'static str,
}

// These prelude functions are runtime builtins rather than stdlib-manifest
// exports. Every parser-recognized macro is sourced from BUILTIN_MACROS below.
const PRELUDE_BUILTINS: &[PreludeBuiltinHelp] = &[
    PreludeBuiltinHelp {
        name: "assert",
        signature: "assert(condition: bool, message?: String)",
        doc: "Panics when condition is false.",
    },
    PreludeBuiltinHelp {
        name: "assert_eq",
        signature: "assert_eq(left, right, message?: String)",
        doc: "Panics when left and right are not equal.",
    },
];

#[allow(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    reason = "REPL loop bundles input, completion, history, and graceful-exit handling"
)]
pub(crate) fn cmd_repl() -> Result<()> {
    use rustyline::error::ReadlineError;
    use rustyline::history::FileHistory;
    use rustyline::{ColorMode, CompletionType, Config, EditMode, Editor};

    use crate::repl_helper::GosReplHelper;

    println!(
        "gos repl - type an expression or declaration\n\
         up/down cycles history · Enter continues until braces close · Ctrl-D or %quit exits"
    );

    let mut transcript: Vec<String> = Vec::new();
    let mut declarations: Vec<String> = Vec::new();
    let mut lets: Vec<String> = Vec::new();
    let mut bindings: Vec<ReplBinding> = Vec::new();
    let mut input_no = 1u32;

    let config = Config::builder()
        .edit_mode(EditMode::Emacs)
        .color_mode(ColorMode::Enabled)
        .completion_type(CompletionType::List)
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
            Err(ReadlineError::Interrupted) => {
                eprintln!("KeyboardInterrupt");
                continue;
            }
            Err(ReadlineError::Eof) => {
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
            let (command, arg) = split_meta_command(rest);
            match command {
                "quit" | "q" => {
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
                "bindings" | "b" => {
                    if bindings.is_empty() {
                        println!("    no `let` bindings yet");
                    } else {
                        let pattern = if arg.is_empty() {
                            None
                        } else {
                            match compile_search_regex("bindings", arg) {
                                Ok(pattern) => Some(pattern),
                                Err(message) => {
                                    eprintln!("{message}");
                                    continue;
                                }
                            }
                        };
                        let lines = render_repl_bindings(&declarations, &lets, &bindings);
                        let matches = lines
                            .iter()
                            .filter(|line| pattern.as_ref().is_none_or(|re| re.is_match(line)))
                            .collect::<Vec<_>>();
                        if matches.is_empty() {
                            println!("    no bindings match `{arg}`");
                            continue;
                        }
                        for (i, line) in matches.into_iter().enumerate() {
                            println!("  {}: {line}", i + 1);
                        }
                    }
                    continue;
                }
                "declarations" | "decls" | "d" => {
                    if declarations.is_empty() {
                        println!("    no declarations yet");
                    } else {
                        let pattern = if arg.is_empty() {
                            None
                        } else {
                            match compile_search_regex("declarations", arg) {
                                Ok(pattern) => Some(pattern),
                                Err(message) => {
                                    eprintln!("{message}");
                                    continue;
                                }
                            }
                        };
                        let matches = declarations
                            .iter()
                            .filter(|line| pattern.as_ref().is_none_or(|re| re.is_match(line)))
                            .collect::<Vec<_>>();
                        if matches.is_empty() {
                            println!("    no declarations match `{arg}`");
                            continue;
                        }
                        for (i, line) in matches.into_iter().enumerate() {
                            println!("  {}: {line}", i + 1);
                        }
                    }
                    continue;
                }
                "reset" | "r" => {
                    declarations.clear();
                    lets.clear();
                    bindings.clear();
                    println!("session cleared");
                    continue;
                }
                "help" | "h" => {
                    match repl_help(arg) {
                        Ok(text) => println!("{text}"),
                        Err(msg) => eprintln!("{msg}"),
                    }
                    continue;
                }
                "ls" | "l" => {
                    match repl_ls(arg) {
                        Ok(text) => println!("{text}"),
                        Err(msg) => eprintln!("{msg}"),
                    }
                    continue;
                }
                "find" | "f" => {
                    match repl_find(arg) {
                        Ok(text) => println!("{text}"),
                        Err(msg) => eprintln!("{msg}"),
                    }
                    continue;
                }
                _ => {
                    eprintln!("unknown meta-command: %{rest}");
                    continue;
                }
            }
        }

        let is_declaration = input_is_declaration(trimmed);

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
            lets.push(candidate.clone());
            let probe_body = format!("{}\n    ()\n", lets.join("\n    "));
            let probe = format!(
                "{}\nfn __irepl_{n}() {{\n    {body}}}\n",
                declarations.join("\n"),
                n = input_no,
                body = probe_body,
            );
            match build_and_call(&probe, &format!("__irepl_{input_no}")) {
                Ok(_) => {
                    update_repl_bindings(&mut bindings, ReplBinding::from_let_source(&candidate));
                    println!("    binding added ({} total)", bindings.len());
                }
                Err(msg) => {
                    lets.pop();
                    eprintln!("    {msg}");
                }
            }
            input_no += 1;
            continue;
        }

        // An assignment (`name = "Mark"`, `count += 1`, ...) mutates a binding
        // from an earlier input. Accumulate it in order with the `let`s so the
        // mutation re-applies before every later input, and run it once now for
        // its effect. A failure (unknown or immutable target) rolls it back and
        // reports the error, leaving the session unchanged.
        if input_is_assignment(trimmed) {
            lets.push(trimmed.to_string());
            let probe_body = format!("{}\n    ()\n", lets.join("\n    "));
            let probe = format!(
                "{}\nfn __irepl_{n}() {{\n    {body}}}\n",
                declarations.join("\n"),
                n = input_no,
                body = probe_body,
            );
            match build_and_call(&probe, &format!("__irepl_{input_no}")) {
                Ok(_) => {}
                Err(msg) => {
                    lets.pop();
                    eprintln!("{}: {msg}", crate::style::error("error"));
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
                        println!(
                            "\x1b[31mOut[{input_no}]:\x1b[0m {}",
                            render_repl_value(&value)
                        );
                    } else {
                        println!("Out[{input_no}]: {}", render_repl_value(&value));
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

/// REPL results use source-like representation, while explicit `print` and
/// `println` retain `Display` formatting. This keeps a bare string distinct
/// from an identifier and applies recursively to aggregate values.
fn render_repl_value(value: &gossamer_interp::Value) -> String {
    value.repr()
}

struct ReplBinding {
    vars: Vec<ReplBindingVar>,
}

struct ReplBindingVar {
    name: String,
    mutable: bool,
}

impl ReplBinding {
    fn from_let_source(source: &str) -> Self {
        Self {
            vars: let_binding_vars(source),
        }
    }
}

fn update_repl_bindings(bindings: &mut Vec<ReplBinding>, new_binding: ReplBinding) {
    if !new_binding.vars.is_empty() {
        for binding in bindings.iter_mut() {
            binding.vars.retain(|var| {
                !new_binding
                    .vars
                    .iter()
                    .any(|new_var| new_var.name == var.name)
            });
        }
        bindings.retain(|binding| !binding.vars.is_empty());
    }
    bindings.push(new_binding);
}

fn render_repl_bindings(
    declarations: &[String],
    lets: &[String],
    bindings: &[ReplBinding],
) -> Vec<String> {
    let let_body = if lets.is_empty() {
        String::new()
    } else {
        format!("{}\n    ", lets.join("\n    "))
    };
    let mut lines = Vec::new();
    for binding in bindings {
        for var in &binding.vars {
            let entry = format!("__irepl_binding_{}", lines.len());
            let source = format!(
                "{}\nfn {entry}() {{ {lets}{name} }}\n",
                declarations.join("\n"),
                lets = let_body,
                name = var.name,
            );
            let value = match build_and_call(&source, &entry) {
                Ok(value) => render_repl_value(&value),
                Err(msg) => format!("<error: {}>", msg.lines().next().unwrap_or("unknown")),
            };
            let prefix = if var.mutable { "mut " } else { "" };
            lines.push(format!("{prefix}{} = {value}", var.name));
        }
    }
    lines
}

fn let_binding_vars(input: &str) -> Vec<ReplBindingVar> {
    use gossamer_ast::{ExprKind, ItemKind, StmtKind};

    let source = format!("fn __irepl_binding_names() {{ {input} }}\n");
    let mut map = gossamer_lex::SourceMap::new();
    let file = map.add_file("irepl-binding-names".to_string(), source.clone());
    let (sf, diags) = gossamer_parse::parse_source_file(&source, file);
    if !diags.is_empty() {
        return Vec::new();
    }
    let Some(item) = sf.items.first() else {
        return Vec::new();
    };
    let ItemKind::Fn(decl) = &item.kind else {
        return Vec::new();
    };
    let Some(body) = &decl.body else {
        return Vec::new();
    };
    let ExprKind::Block(block) = &body.kind else {
        return Vec::new();
    };
    let Some(stmt) = block.stmts.first() else {
        return Vec::new();
    };
    let StmtKind::Let { pattern, .. } = &stmt.kind else {
        return Vec::new();
    };
    let mut vars = Vec::new();
    collect_repl_pattern_bindings(pattern, &mut vars);
    vars
}

fn collect_repl_pattern_bindings(pattern: &gossamer_ast::Pattern, out: &mut Vec<ReplBindingVar>) {
    use gossamer_ast::PatternKind;

    match &pattern.kind {
        PatternKind::Ident {
            mutability,
            name,
            subpattern,
        } => {
            out.push(ReplBindingVar {
                name: name.name.clone(),
                mutable: mutability.is_mutable(),
            });
            if let Some(subpattern) = subpattern {
                collect_repl_pattern_bindings(subpattern, out);
            }
        }
        PatternKind::Tuple(patterns) => {
            for pattern in patterns {
                collect_repl_pattern_bindings(pattern, out);
            }
        }
        PatternKind::Or(patterns) => {
            if let Some(pattern) = patterns.first() {
                collect_repl_pattern_bindings(pattern, out);
            }
        }
        PatternKind::Slice {
            prefix,
            rest,
            suffix,
        } => {
            for pattern in prefix {
                collect_repl_pattern_bindings(pattern, out);
            }
            if let Some(rest) = rest {
                collect_repl_pattern_bindings(rest, out);
            }
            for pattern in suffix {
                collect_repl_pattern_bindings(pattern, out);
            }
        }
        PatternKind::Struct { fields, .. } => {
            for field in fields {
                match &field.pattern {
                    Some(pattern) => collect_repl_pattern_bindings(pattern, out),
                    None => out.push(ReplBindingVar {
                        name: field.name.name.clone(),
                        mutable: false,
                    }),
                }
            }
        }
        PatternKind::TupleStruct { elems, .. } => {
            for pattern in elems {
                collect_repl_pattern_bindings(pattern, out);
            }
        }
        PatternKind::Ref { inner, .. } => collect_repl_pattern_bindings(inner, out),
        _ => {}
    }
}

fn split_meta_command(input: &str) -> (&str, &str) {
    input
        .split_once(char::is_whitespace)
        .map_or((input, ""), |(command, arg)| (command, arg.trim()))
}

fn input_is_declaration(input: &str) -> bool {
    let input = input
        .strip_prefix("pub ")
        .or_else(|| input.strip_prefix("pub(crate) "))
        .unwrap_or(input);
    input.starts_with("fn ")
        || input.starts_with("struct ")
        || input.starts_with("enum ")
        || input.starts_with("use ")
        || input.starts_with("const ")
        || input.starts_with("static ")
        || input.starts_with("type ")
}

fn repl_help(arg: &str) -> std::result::Result<String, String> {
    if arg.is_empty() {
        return Ok(REPL_HELP_TEXT.to_string());
    }
    if let Some(pattern) = regex_argument(arg)? {
        return Ok(render_help_matches(&pattern));
    }

    let query = normalize_query(arg);
    let mut out = String::new();
    for builtin in matching_builtin_macros(query) {
        push_builtin_macro_help(&mut out, builtin);
    }
    for builtin in matching_prelude_builtins(query) {
        push_prelude_builtin_help(&mut out, builtin);
    }
    for module in matching_modules(query) {
        push_module_help(&mut out, &module);
    }
    for (module, item) in matching_items(query) {
        push_item_help(&mut out, &module, &item);
    }
    for feature in matching_features(query) {
        push_feature_help(&mut out, feature);
    }

    if out.is_empty() {
        Ok(format!("no help found for `{arg}`"))
    } else {
        Ok(out.trim_end().to_string())
    }
}

fn repl_ls(arg: &str) -> std::result::Result<String, String> {
    if arg.is_empty() {
        return Ok(render_module_dir(gossamer_std::registry::modules()));
    }
    if let Some(pattern) = regex_argument(arg)? {
        return Ok(render_dir_matches(&pattern));
    }

    let query = normalize_query(arg);
    let modules = matching_modules(query);
    if !modules.is_empty() {
        return Ok(render_module_dir(&modules));
    }

    if let Some((module, item)) = matching_items(query).into_iter().next() {
        return Err(format!(
            "`{}::{}` is a {}; %ls accepts module names only (use %help for an item)",
            module.path,
            item.name,
            item_kind_label(item.kind)
        ));
    }
    Ok(format!("no stdlib module found for `{arg}`"))
}

/// Searches public symbol paths with a regular expression. Plain text remains
/// a substring search, while regex operators can broaden or constrain the
/// match.
fn repl_find(arg: &str) -> std::result::Result<String, String> {
    let query = normalize_query(arg);
    if query.is_empty() {
        return Err("usage: %find <name-regex>".to_string());
    }
    let pattern = compile_search_regex("find", query)?;

    let mut matches = Vec::new();
    for module in gossamer_std::registry::modules() {
        let candidate = FindCandidate {
            path: module.path.to_string(),
            kind: "module",
            doc: module.summary,
        };
        if pattern.is_match(&candidate.path) {
            matches.push(candidate);
        }
        for item in module.items {
            let candidate = FindCandidate {
                path: format!("{}::{}", module.path, item.name),
                kind: item_kind_label(item.kind),
                doc: item.doc,
            };
            if pattern.is_match(&candidate.path) {
                matches.push(candidate);
            }
        }
    }
    for builtin in BUILTIN_MACROS {
        let candidate = FindCandidate {
            path: builtin.name.to_string(),
            kind: "macro",
            doc: builtin.doc,
        };
        if pattern.is_match(&candidate.path) {
            matches.push(candidate);
        }
    }
    for builtin in PRELUDE_BUILTINS {
        let candidate = FindCandidate {
            path: builtin.name.to_string(),
            kind: "builtin",
            doc: builtin.doc,
        };
        if pattern.is_match(&candidate.path) {
            matches.push(candidate);
        }
    }

    matches.sort_by(|left, right| left.path.cmp(&right.path));
    matches.dedup_by(|left, right| left.path == right.path && left.kind == right.kind);

    if matches.is_empty() {
        return Ok(format!("no public symbols found for `{query}`"));
    }
    let mut out = String::new();
    for candidate in matches.into_iter().take(50) {
        out.push_str(&format!(
            "{:<48} {:<7} {}\n",
            candidate.path, candidate.kind, candidate.doc
        ));
    }
    Ok(out.trim_end().to_string())
}

struct FindCandidate<'a> {
    path: String,
    kind: &'a str,
    doc: &'a str,
}

fn compile_search_regex(command: &str, query: &str) -> std::result::Result<Regex, String> {
    Regex::new(query).map_err(|error| format!("invalid {command} regex `{query}`: {error}"))
}

fn regex_argument(arg: &str) -> std::result::Result<Option<Regex>, String> {
    if !(arg.starts_with('/') && arg.ends_with('/') && arg.len() >= 2) {
        return Ok(None);
    }
    Regex::new(&arg[1..arg.len() - 1])
        .map(Some)
        .map_err(|e| format!("invalid regex `{arg}`: {e}"))
}

fn render_help_matches(pattern: &Regex) -> String {
    let mut out = String::new();
    for builtin in BUILTIN_MACROS {
        if pattern.is_match(builtin.name)
            || pattern.is_match(builtin.signature)
            || pattern.is_match(builtin.doc)
        {
            push_builtin_macro_help(&mut out, builtin);
        }
    }
    for builtin in PRELUDE_BUILTINS {
        if pattern.is_match(builtin.name)
            || pattern.is_match(builtin.signature)
            || pattern.is_match(builtin.doc)
        {
            push_prelude_builtin_help(&mut out, builtin);
        }
    }
    for module in gossamer_std::registry::modules() {
        if module_matches_regex(pattern, module) {
            push_module_help(&mut out, module);
        }
        for item in module.items {
            if item_matches_regex(pattern, module, item) {
                push_item_help(&mut out, module, item);
            }
        }
    }
    for feature in gossamer_std::manifest::feature_status::all_entries() {
        if !is_stdlib_module_path(feature.path)
            && (pattern.is_match(feature.path) || pattern.is_match(feature.doc))
        {
            push_feature_help(&mut out, feature);
        }
    }
    if out.is_empty() {
        "no help matches".to_string()
    } else {
        out.trim_end().to_string()
    }
}

fn render_dir_matches(pattern: &Regex) -> String {
    let mut lines = Vec::new();
    for module in gossamer_std::registry::modules() {
        if module_matches_regex(pattern, module) {
            let mut line = String::new();
            push_module_dir_line(&mut line, module);
            lines.push(line);
        }
    }
    if lines.is_empty() {
        "no stdlib modules match".to_string()
    } else {
        lines.sort_unstable();
        lines.concat().trim_end().to_string()
    }
}

fn render_module_dir(modules: &[StdModule]) -> String {
    let mut out = String::new();
    for module in modules {
        push_module_dir_line(&mut out, module);
        if modules.len() == 1 {
            // A directory command names a module, so render its complete
            // namespace tree: the module's own members plus every registered
            // descendant module and its members.  A plain `%ls` deliberately
            // stays shallow; recursively expanding the entire standard
            // library there would make the useful module overview unusable.
            push_module_items(&mut out, module);
            let prefix = format!("{}::", module.path);
            for child in gossamer_std::registry::modules()
                .iter()
                .filter(|child| child.path.starts_with(&prefix))
            {
                push_module_dir_line(&mut out, child);
                push_module_items(&mut out, child);
            }
        }
    }
    let mut lines = out.lines().collect::<Vec<_>>();
    lines.sort_unstable();
    lines.join("\n")
}

fn push_module_items(out: &mut String, module: &StdModule) {
    for item in module.items {
        push_item_dir(out, module, item);
    }
}

fn push_module_help(out: &mut String, module: &StdModule) {
    out.push_str(&format!("{}\n", module.path));
    out.push_str(&format!("  {}\n", module.summary));
    out.push_str(&format!("  items: {}\n\n", module.items.len()));
}

fn push_item_help(out: &mut String, module: &StdModule, item: &StdItem) {
    out.push_str(&format!(
        "{}::{} [{}]\n",
        module.path,
        item.name,
        item_kind_label(item.kind)
    ));
    if let Some(signature) = gossamer_types::stdlib_function_signature(module.path, item.name) {
        out.push_str(&format!("  {signature}\n"));
    }
    out.push_str(&format!("  {}\n\n", item.doc));
}

fn push_builtin_macro_help(out: &mut String, builtin: &BuiltinMacro) {
    out.push_str(&format!("{} [macro]\n", builtin.name));
    out.push_str(&format!("  {}\n", builtin.signature));
    out.push_str(&format!("  {}\n\n", builtin.doc));
}

fn push_prelude_builtin_help(out: &mut String, builtin: &PreludeBuiltinHelp) {
    out.push_str(&format!("{} [builtin]\n", builtin.name));
    out.push_str(&format!("  {}\n", builtin.signature));
    out.push_str(&format!("  {}\n\n", builtin.doc));
}

fn push_feature_help(out: &mut String, feature: gossamer_std::manifest::FeatureStatus) {
    out.push_str(&format!("{} ({})\n", feature.path, feature.status.tag()));
    out.push_str(&format!("  {}\n\n", feature.doc));
}

fn push_module_dir_line(out: &mut String, module: &StdModule) {
    out.push_str(&format!("{:<32} module  {}\n", module.path, module.summary));
}

fn push_item_dir(out: &mut String, module: &StdModule, item: &StdItem) {
    out.push_str(&format!(
        "{:<32} {:<6} {}\n",
        format!("{}::{}", module.path, item.name),
        item_kind_label(item.kind),
        item.doc
    ));
}

fn matching_modules(query: &str) -> Vec<StdModule> {
    gossamer_std::registry::modules()
        .iter()
        .copied()
        .filter(|module| module_query_matches(module, query))
        .collect()
}

fn matching_items(query: &str) -> Vec<(StdModule, StdItem)> {
    let mut out = Vec::new();
    for module in gossamer_std::registry::modules() {
        for item in module.items {
            if item_query_matches(module, item, query) {
                out.push((*module, *item));
            }
        }
    }
    out
}

fn matching_features(query: &str) -> Vec<gossamer_std::manifest::FeatureStatus> {
    gossamer_std::manifest::feature_status::all_entries()
        .into_iter()
        .filter(|entry| !is_stdlib_module_path(entry.path))
        .filter(|entry| feature_query_matches(entry.path, query))
        .collect()
}

fn matching_builtin_macros(query: &str) -> Vec<&'static BuiltinMacro> {
    BUILTIN_MACROS
        .iter()
        .filter(|builtin| builtin.name == query)
        .collect()
}

fn matching_prelude_builtins(query: &str) -> Vec<&'static PreludeBuiltinHelp> {
    PRELUDE_BUILTINS
        .iter()
        .filter(|builtin| builtin.name == query)
        .collect()
}

fn is_stdlib_module_path(path: &str) -> bool {
    gossamer_std::registry::module(path).is_some()
}

fn module_query_matches(module: &StdModule, query: &str) -> bool {
    module_aliases(module.path).contains(&query)
}

fn item_query_matches(module: &StdModule, item: &StdItem, query: &str) -> bool {
    if item.name == query {
        return true;
    }
    module_aliases(module.path)
        .iter()
        .any(|alias| format!("{alias}::{}", item.name) == query)
}

fn feature_query_matches(path: &str, query: &str) -> bool {
    if path == query {
        return true;
    }
    let stripped = path
        .strip_prefix("lang::")
        .or_else(|| path.strip_prefix("std::"))
        .unwrap_or(path);
    stripped == query || path.rsplit("::").next().is_some_and(|last| last == query)
}

fn module_matches_regex(pattern: &Regex, module: &StdModule) -> bool {
    pattern.is_match(module.path) || pattern.is_match(module.summary)
}

fn item_matches_regex(pattern: &Regex, module: &StdModule, item: &StdItem) -> bool {
    pattern.is_match(&format!("{}::{}", module.path, item.name))
        || pattern.is_match(item.name)
        || pattern.is_match(item.doc)
}

fn module_aliases(path: &'static str) -> Vec<&'static str> {
    let mut aliases = vec![path];
    if let Some(stripped) = path.strip_prefix("std::") {
        aliases.push(stripped);
    }
    if let Some(last) = path.rsplit("::").next()
        && !aliases.contains(&last)
    {
        aliases.push(last);
    }
    aliases
}

fn normalize_query(arg: &str) -> &str {
    arg.trim_matches('`').trim()
}

fn item_kind_label(kind: StdItemKind) -> &'static str {
    match kind {
        StdItemKind::Function => "fn",
        StdItemKind::Type => "type",
        StdItemKind::Trait => "trait",
        StdItemKind::Macro => "macro",
        StdItemKind::Const => "const",
    }
}

/// True when `input` is a single assignment statement (`x = e`, `x += e`,
/// `x.f = e`, `x[i] = e`, `*x = e`). Such a statement mutates a binding
/// introduced by an earlier input; the REPL accumulates it alongside the
/// `let`s so the write survives into later inputs, rather than applying it in a
/// throwaway frame that is then discarded. Parsing (instead of scanning for an
/// `=`) keeps `==` / `<=` comparisons and `let` initializers from being misread
/// as assignments.
fn input_is_assignment(input: &str) -> bool {
    use gossamer_ast::{ExprKind, ItemKind, StmtKind};
    let source = format!("fn __irepl_classify() {{ {input} }}\n");
    let mut map = gossamer_lex::SourceMap::new();
    let file = map.add_file("irepl-classify".to_string(), source.clone());
    let (sf, diags) = gossamer_parse::parse_source_file(&source, file);
    if !diags.is_empty() {
        return false;
    }
    let Some(item) = sf.items.first() else {
        return false;
    };
    let ItemKind::Fn(decl) = &item.kind else {
        return false;
    };
    let Some(body) = &decl.body else {
        return false;
    };
    let ExprKind::Block(block) = &body.kind else {
        return false;
    };
    // A bare `x = e` (no trailing `;`) parses as the block's tail expression;
    // `x = e;` parses as the final statement. Check whichever carries the value.
    let target = block.tail.as_deref().or_else(|| match block.stmts.last() {
        Some(stmt) => match &stmt.kind {
            StmtKind::Expr { expr, .. } => Some(expr.as_ref()),
            _ => None,
        },
        None => None,
    });
    matches!(target.map(|e| &e.kind), Some(ExprKind::Assign { .. }))
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
    let (sf, parse_diags) = gossamer_parse::autoderive::parse_with_autoderive(&source, file);
    if !parse_diags.is_empty() {
        return Err(format_parse_diags(&parse_diags, &map, file));
    }
    let (res, resolve_diags) = gossamer_resolve::resolve_source_file(&sf);
    if !resolve_diags.is_empty() {
        return Err(format_semantic_diags("resolution", &resolve_diags));
    }
    let mut tcx = gossamer_types::TyCtxt::new();
    let (tbl, type_diags) = gossamer_types::typecheck_source_file(&sf, &res, &mut tcx);
    if !type_diags.is_empty() {
        return Err(format_semantic_diags("type", &type_diags));
    }
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
    let (sf, parse_diags) = gossamer_parse::autoderive::parse_with_autoderive(source, file);
    if !parse_diags.is_empty() {
        return Err(format_parse_diags(&parse_diags, &map, file));
    }
    let (res, resolve_diags) = gossamer_resolve::resolve_source_file(&sf);
    if !resolve_diags.is_empty() {
        return Err(format_semantic_diags("resolution", &resolve_diags));
    }
    let mut tcx = gossamer_types::TyCtxt::new();
    let (tbl, type_diags) = gossamer_types::typecheck_source_file(&sf, &res, &mut tcx);
    // REPL expressions are installed as the tail of a generated function with
    // no written return annotation. The REPL deliberately returns that value
    // for `Out[N]`, so its tail is neither discarded nor a user error.
    // Suppress only that exact generated-body diagnostic, never one from the
    // submitted expression's children or declarations.
    let tail_span = repl_generated_body_span(&sf);
    let user_type_diags: Vec<_> = type_diags
        .iter()
        .filter(|diag| !is_implicit_repl_tail_diag(diag, tail_span))
        .collect();
    if !user_type_diags.is_empty() {
        return Err(format_semantic_diags("type", &user_type_diags));
    }
    let program = gossamer_hir::lower_source_file(&sf, &res, &tbl, &mut tcx);
    let mut vm = gossamer_interp::Vm::new();
    vm.load(&program, tcx, true).map_err(|e| format!("{e}"))?;
    vm.call(entry, Vec::new()).map_err(|e| format!("{e}"))
}

fn repl_generated_body_span(sf: &gossamer_ast::SourceFile) -> Option<gossamer_lex::Span> {
    use gossamer_ast::ItemKind;

    sf.items.iter().find_map(|item| {
        let ItemKind::Fn(decl) = &item.kind else {
            return None;
        };
        if !decl.name.name.starts_with("__irepl_") {
            return None;
        }
        decl.body.as_ref().map(|body| body.span)
    })
}

fn is_implicit_repl_tail_diag(
    diag: &gossamer_types::TypeDiagnostic,
    tail_span: Option<gossamer_lex::Span>,
) -> bool {
    match (&diag.error, tail_span) {
        (gossamer_types::TypeError::TypeMismatch { expected, .. }, Some(span)) => {
            expected == "()" && diag.span == span
        }
        (gossamer_types::TypeError::DiscardedResult, Some(span)) => diag.span == span,
        _ => false,
    }
}

/// Renders hard resolver/type-checker failures before the REPL can lower a
/// program.  Keeping this gate here is essential: lowering after a rejected
/// call used to let missing or wrongly typed arguments reach permissive
/// runtime shims, which then silently substituted defaults.
fn format_semantic_diags<T: std::fmt::Display>(phase: &str, diags: &[T]) -> String {
    let noun = if diags.len() == 1 { "error" } else { "errors" };
    let mut out = format!("{} {phase} {noun}:\n", diags.len());
    for diag in diags {
        out.push_str("  ");
        out.push_str(&diag.to_string());
        out.push('\n');
    }
    out.pop();
    out
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
