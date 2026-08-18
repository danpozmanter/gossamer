//! Rustyline `Helper` that paints Gossamer source with ANSI colour
//! escapes as the user types in the REPL.
//! Lexing is delegated to `gossamer_lex::tokenize`; the helper does
//! not build an AST, so partially-typed input (unterminated strings,
//! dangling punctuation) still paints correctly up to the last
//! lexable boundary.

#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use gossamer_lex::{Punct, SourceMap, TokenKind, tokenize};
use rustyline::completion::Completer;
use rustyline::highlight::{CmdKind, Highlighter};
use rustyline::hint::Hinter;
use rustyline::validate::{ValidationContext, ValidationResult, Validator};
use rustyline::{Cmd, ConditionalEventHandler, Event, EventContext, Helper, RepeatCount};

/// ANSI colour escapes used by the REPL. Chosen to read well on both
/// light and dark terminals; dim for comments keeps them present but
/// low-contrast.
const RESET: &str = "\x1b[0m";
const CYAN_BOLD: &str = "\x1b[1;36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const MAGENTA: &str = "\x1b[35m";
const DIM: &str = "\x1b[2m";

/// REPL helper that highlights Gossamer source as the user types.
#[derive(Default)]
pub(crate) struct GosReplHelper {
    binding_method_owners: HashMap<String, String>,
    session_type_names: HashSet<String>,
}

impl GosReplHelper {
    /// Constructs a fresh helper with no per-session state.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Replaces the set of type names the session declares, so the
    /// highlighter paints a type where the editor's semantic tokens would
    /// and leaves every other identifier alone.
    pub(crate) fn set_session_type_names(&mut self, names: HashSet<String>) {
        self.session_type_names = names;
    }

    /// Whether `name` names a type here: one the session declared, or one
    /// the language and its standard library provide.
    fn is_type_name(&self, name: &str) -> bool {
        self.session_type_names.contains(name) || is_known_type_name(name)
    }

    /// Records the core method receiver owner for a persistent binding.
    pub(crate) fn set_binding_method_owner(&mut self, name: &str, owner: Option<&str>) {
        if let Some(owner) = owner {
            self.binding_method_owners
                .insert(name.to_string(), owner.to_string());
        } else {
            self.binding_method_owners.remove(name);
        }
    }

    /// Removes completion metadata for a binding ended by `%drop`.
    pub(crate) fn forget_binding(&mut self, name: &str) {
        self.binding_method_owners.remove(name);
    }

    /// Clears all completion metadata with the rest of the REPL session.
    pub(crate) fn reset_session(&mut self) {
        self.binding_method_owners.clear();
        self.session_type_names.clear();
    }
}

/// Type names the language itself provides: the primitives, the core
/// generic containers, and the shapes syntax alone builds. A name outside
/// this set and outside the session's own declarations is left uncoloured,
/// matching the editor, which paints a type only where one resolves.
fn is_known_type_name(name: &str) -> bool {
    matches!(
        name,
        "bool"
            | "char"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "usize"
            | "f32"
            | "f64"
            | "String"
            | "Vec"
            | "Map"
            | "BTreeMap"
            | "Set"
            | "BTreeSet"
            | "Deque"
            | "Queue"
            | "Stack"
            | "MaxHeap"
            | "MinHeap"
            | "Option"
            | "Result"
            | "Box"
            | "Arc"
            | "Rc"
            | "Weak"
            | "Sender"
            | "Receiver"
            | "JoinHandle"
            | "Self"
    ) || STDLIB_TYPE_NAMES.with(|names| names.contains(name))
}

thread_local! {
    /// Type names the standard library exports, read once from its manifest.
    static STDLIB_TYPE_NAMES: HashSet<String> = gossamer_std::registry::modules()
        .iter()
        .flat_map(|module| module.items)
        .filter(|item| matches!(item.kind, gossamer_std::registry::StdItemKind::Type))
        .map(|item| item.name.to_string())
        .collect();
}

impl Helper for GosReplHelper {}

/// Adds language-aware indentation when Enter extends incomplete input.
pub(crate) struct ReplEnterHandler;

impl ConditionalEventHandler for ReplEnterHandler {
    fn handle(
        &self,
        _evt: &Event,
        _n: RepeatCount,
        _positive: bool,
        ctx: &EventContext<'_>,
    ) -> Option<Cmd> {
        let input = ctx.line();
        if ctx.pos() != input.len() || incomplete_reason(input).is_none() {
            return None;
        }
        Some(Cmd::Insert(1, format!("\n{}", continuation_indent(input))))
    }
}

fn continuation_indent(input: &str) -> String {
    const INDENT: &str = "    ";

    let current_line = input.rsplit('\n').next().unwrap_or(input);
    let leading = &current_line[..current_line
        .find(|ch: char| !ch.is_whitespace())
        .unwrap_or(current_line.len())];

    let mut map = SourceMap::new();
    let file = map.add_file("repl-indent.gos", input.to_string());
    let (tokens, _) = tokenize(input, file);
    let opens_block = tokens
        .iter()
        .rev()
        .find(|token| {
            !matches!(
                token.kind,
                TokenKind::Eof
                    | TokenKind::Whitespace
                    | TokenKind::LineComment
                    | TokenKind::BlockComment
            )
        })
        .is_some_and(|token| {
            matches!(
                token.kind,
                TokenKind::Punct(Punct::LBrace | Punct::LParen | Punct::LBracket)
            )
        });

    if opens_block {
        format!("{leading}{INDENT}")
    } else {
        leading.to_string()
    }
}

/// Every Gossamer keyword, completed when the cursor word is unqualified.
/// Mirrors `gossamer_lex::Keyword`; the enum exposes no all-variants
/// iterator, so the set is listed here and kept in step with the lexer.
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "defer", "else", "enum",
    "extern", "false", "fn", "for", "go", "if", "impl", "in", "let", "loop", "match", "mod", "mut",
    "package", "pub", "return", "select", "self", "static", "struct", "super", "trait", "true",
    "type", "unsafe", "use", "where", "while", "yield",
];

impl Completer for GosReplHelper {
    type Candidate = String;

    /// Completes the identifier-or-path word ending at the cursor against
    /// the keyword set and the standard-library surface (`std::registry`):
    /// module paths and their `module::item` members. Returns the byte
    /// offset where the replacement begins plus the prefix-matching
    /// candidates, sorted and de-duplicated.
    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<String>)> {
        Ok(complete_at(line, pos, &self.binding_method_owners))
    }
}

/// Computes completion candidates for the word ending at `pos`. Split out
/// from the trait method so it is testable without a rustyline `Context`.
fn complete_at(
    line: &str,
    pos: usize,
    binding_method_owners: &HashMap<String, String>,
) -> (usize, Vec<String>) {
    let start = word_start(line, pos);
    let word = &line[start..pos];
    // A cursor sitting straight after the dot has no word yet, and that is
    // exactly when the whole method surface is worth offering. Only the
    // keyword and module completions below need something to match against.
    if start > 0
        && line.as_bytes()[start - 1] == b'.'
        && let Some(receiver_start) = receiver_start(line, start - 1)
        && let Some(owner) = binding_method_owners.get(&line[receiver_start..start - 1])
    {
        return (
            start,
            crate::repl::core_method_names(owner)
                .into_iter()
                .filter(|method| method.starts_with(word))
                .map(str::to_string)
                .collect(),
        );
    }
    if word.is_empty() {
        return (start, Vec::new());
    }
    let mut out: Vec<String> = Vec::new();
    if !word.contains(':') {
        out.extend(
            KEYWORDS
                .iter()
                .filter(|kw| kw.starts_with(word))
                .map(|kw| (*kw).to_string()),
        );
    }
    for module in gossamer_std::registry::modules() {
        for prefix in module_prefixes(module.path) {
            if prefix.starts_with(word) {
                out.push(prefix.to_string());
            }
            for item in module.items {
                let qualified = format!("{prefix}::{}", item.name);
                if qualified.starts_with(word) {
                    out.push(qualified);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    (start, out)
}

fn receiver_start(line: &str, dot: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut start = dot;
    while start > 0 {
        let byte = bytes[start - 1];
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            start -= 1;
        } else {
            break;
        }
    }
    (start < dot).then_some(start)
}

/// The module path forms a user might type to reach a module: the canonical
/// path (`std::strings`), the `std::`-stripped form used after `use std::…`
/// (`strings`, `encoding::json`), and the bare last segment (`json`).
fn module_prefixes(path: &'static str) -> Vec<&'static str> {
    let mut forms = vec![path];
    if let Some(stripped) = path.strip_prefix("std::") {
        if !forms.contains(&stripped) {
            forms.push(stripped);
        }
    }
    if let Some(last) = path.rsplit("::").next() {
        if !forms.contains(&last) {
            forms.push(last);
        }
    }
    forms
}

/// Byte offset of the start of the identifier-or-path word ending at `pos`.
/// Walks back over identifier bytes and `:` path separators so a
/// partially-typed `strings::sp` completes as a single unit. Only ASCII
/// identifier bytes are consumed, so `start` lands on a char boundary.
fn word_start(line: &str, pos: usize) -> usize {
    let bytes = line.as_bytes();
    let mut start = pos;
    while start > 0 {
        let c = bytes[start - 1];
        if c.is_ascii_alphanumeric() || c == b'_' || c == b':' {
            start -= 1;
        } else {
            break;
        }
    }
    start
}

impl Hinter for GosReplHelper {
    type Hint = String;
}

impl Validator for GosReplHelper {
    fn validate(&self, ctx: &mut ValidationContext<'_>) -> rustyline::Result<ValidationResult> {
        let input = ctx.input();
        if input.is_empty() {
            return Ok(ValidationResult::Valid(None));
        }
        if incomplete_reason(input).is_some() {
            return Ok(ValidationResult::Incomplete);
        }
        Ok(ValidationResult::Valid(None))
    }
}

/// Returns `Some(_)` when `input` is syntactically incomplete - an
/// unclosed brace/paren/bracket, or a trailing unterminated block
/// comment. In that case the REPL keeps reading subsequent lines as
/// a continuation of the same expression.
fn incomplete_reason(input: &str) -> Option<&'static str> {
    // Meta-command arguments may themselves be incomplete Gossamer syntax,
    // especially regexes such as `[` or `foo(`. Submit them immediately so
    // the command can either use the pattern or report its regex error.
    if input.trim_start().starts_with('%') {
        return None;
    }
    let mut map = SourceMap::new();
    let file = map.add_file("repl.gos", input.to_string());
    let (tokens, lex_errors) = tokenize(input, file);
    for err in &lex_errors {
        let message = format!("{err:?}");
        if message.contains("Unterminated") {
            return Some("unterminated literal or comment");
        }
    }
    let mut depth_brace: i32 = 0;
    let mut depth_paren: i32 = 0;
    let mut depth_bracket: i32 = 0;
    for token in tokens {
        match token.kind {
            TokenKind::Punct(Punct::LBrace) => depth_brace += 1,
            TokenKind::Punct(Punct::RBrace) => depth_brace -= 1,
            TokenKind::Punct(Punct::LParen) => depth_paren += 1,
            TokenKind::Punct(Punct::RParen) => depth_paren -= 1,
            TokenKind::Punct(Punct::LBracket) => depth_bracket += 1,
            TokenKind::Punct(Punct::RBracket) => depth_bracket -= 1,
            _ => {}
        }
    }
    if depth_brace > 0 {
        return Some("unbalanced `{`");
    }
    if depth_paren > 0 {
        return Some("unbalanced `(`");
    }
    if depth_bracket > 0 {
        return Some("unbalanced `[`");
    }
    None
}

impl Highlighter for GosReplHelper {
    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> Cow<'l, str> {
        if line.is_empty() {
            return Cow::Borrowed(line);
        }
        let mut map = SourceMap::new();
        let file = map.add_file("repl.gos", line.to_string());
        let (tokens, _) = tokenize(line, file);
        let mut out = String::with_capacity(line.len() + tokens.len() * 6);
        let mut cursor = 0usize;
        for token in tokens {
            let start = token.span.start as usize;
            let end = token.span.end as usize;
            if start > cursor {
                out.push_str(&line[cursor..start]);
            }
            if end <= start || end > line.len() {
                cursor = start.max(cursor);
                continue;
            }
            let text = &line[start..end];
            match token.kind {
                TokenKind::Eof => {}
                TokenKind::Keyword(_) => {
                    out.push_str(CYAN_BOLD);
                    out.push_str(text);
                    out.push_str(RESET);
                }
                TokenKind::StringLit
                | TokenKind::RawStringLit { .. }
                | TokenKind::TripleStringLit
                | TokenKind::ByteStringLit
                | TokenKind::RawByteStringLit { .. }
                | TokenKind::CharLit
                | TokenKind::ByteLit => {
                    out.push_str(GREEN);
                    out.push_str(text);
                    out.push_str(RESET);
                }
                TokenKind::IntLit | TokenKind::FloatLit => {
                    out.push_str(YELLOW);
                    out.push_str(text);
                    out.push_str(RESET);
                }
                TokenKind::LineComment | TokenKind::BlockComment => {
                    out.push_str(DIM);
                    out.push_str(text);
                    out.push_str(RESET);
                }
                TokenKind::Ident => {
                    if self.is_type_name(text) {
                        out.push_str(MAGENTA);
                        out.push_str(text);
                        out.push_str(RESET);
                    } else {
                        out.push_str(text);
                    }
                }
                TokenKind::Label => {
                    out.push_str(CYAN_BOLD);
                    out.push_str(text);
                    out.push_str(RESET);
                }
                TokenKind::Punct(_) | TokenKind::Whitespace | TokenKind::Invalid => {
                    out.push_str(text);
                }
            }
            cursor = end.max(cursor);
        }
        if cursor < line.len() {
            out.push_str(&line[cursor..]);
        }
        Cow::Owned(out)
    }

    fn highlight_char(&self, _line: &str, _pos: usize, _kind: CmdKind) -> bool {
        true
    }
}

#[cfg(test)]
mod repl_helper_tests {
    use super::{GosReplHelper, MAGENTA, complete_at, continuation_indent, incomplete_reason};
    use rustyline::highlight::Highlighter;
    use std::collections::{HashMap, HashSet};

    /// The editor paints a type where one resolves, so the REPL does too: a
    /// capitalised word nothing declares is an ordinary identifier, and
    /// colouring it would claim a resolution the session does not have.
    #[test]
    fn only_a_name_that_resolves_to_a_type_is_painted_as_one() {
        let mut helper = GosReplHelper::new();
        helper.set_session_type_names(HashSet::from(["Point".to_string()]));

        let painted = helper.highlight("Point", 0);
        assert!(
            painted.contains(MAGENTA),
            "a session-declared type is painted: {painted:?}"
        );

        let builtin = helper.highlight("Vec", 0);
        assert!(
            builtin.contains(MAGENTA),
            "a language type is painted: {builtin:?}"
        );

        let unknown = helper.highlight("Bogus", 0);
        assert!(
            !unknown.contains(MAGENTA),
            "a capitalised name nothing declares is left alone: {unknown:?}"
        );

        helper.reset_session();
        let dropped = helper.highlight("Point", 0);
        assert!(
            !dropped.contains(MAGENTA),
            "clearing the session forgets its types: {dropped:?}"
        );
    }

    /// A cursor sitting straight after the dot is the moment the whole method
    /// surface is worth offering, so an empty prefix lists every method the
    /// receiver has rather than nothing.
    #[test]
    fn a_bare_dot_offers_the_receivers_methods() {
        let mut owners = HashMap::new();
        owners.insert("x".to_string(), "String".to_string());
        let (start, candidates) = complete_at("x.", 2, &owners);
        assert_eq!(start, 2, "the replacement begins after the dot");
        assert!(
            !candidates.is_empty(),
            "a bare dot on a String binding offers its methods"
        );
        assert!(
            candidates.iter().any(|m| m == "len"),
            "String methods are offered: {candidates:?}"
        );
        // A prefix after the dot still narrows the same surface.
        let (narrow_start, narrowed) = complete_at("x.le", 4, &owners);
        assert_eq!(narrow_start, 2);
        assert!(narrowed.iter().any(|m| m == "len"), "{narrowed:?}");
        assert!(
            narrowed.len() < candidates.len(),
            "a prefix narrows the candidate list"
        );
        // An unknown receiver still completes nothing rather than guessing.
        let (_, unknown) = complete_at("zzz.", 4, &owners);
        assert!(unknown.is_empty(), "{unknown:?}");
    }

    #[test]
    fn keyword_prefix_completes() {
        let (start, cands) = complete_at("le", 2, &HashMap::new());
        assert_eq!(start, 0);
        assert!(cands.iter().any(|c| c == "let"));
    }

    #[test]
    fn empty_word_yields_nothing() {
        let (_, cands) = complete_at("let x = ", 8, &HashMap::new());
        assert!(cands.is_empty());
    }

    #[test]
    fn qualified_path_completes_member() {
        let (start, cands) = complete_at("println!(strings::jo", 20, &HashMap::new());
        assert_eq!(start, 9);
        assert!(
            cands.iter().any(|c| c == "strings::join"),
            "expected strings::join in {cands:?}"
        );
        // A qualified word must not pull in keyword candidates.
        assert!(cands.iter().all(|c| c.contains("::")));
    }

    #[test]
    fn completion_offset_is_word_start_not_line_start() {
        let (start, _) = complete_at("    fo", 6, &HashMap::new());
        assert_eq!(start, 4);
    }

    #[test]
    fn set_binding_completes_methods_after_dot() {
        let mut helper = GosReplHelper::new();
        helper.set_binding_method_owner("set", Some("Set"));

        let (start, cands) = complete_at("set.ins", 7, &helper.binding_method_owners);
        assert_eq!(start, 4);
        assert_eq!(cands, vec!["insert"]);

        let (_, cands) = complete_at("set.insecure", 12, &helper.binding_method_owners);
        assert!(cands.is_empty());
    }

    #[test]
    fn stack_binding_completes_methods_after_dot() {
        let mut helper = GosReplHelper::new();
        helper.set_binding_method_owner("x", Some("Stack"));

        let (start, cands) = complete_at("x.l", 3, &helper.binding_method_owners);
        assert_eq!(start, 2);
        assert_eq!(cands, vec!["len"]);
    }

    #[test]
    fn meta_command_regex_is_never_treated_as_incomplete_source() {
        assert_eq!(incomplete_reason("%info ["), None);
        assert_eq!(incomplete_reason("%bindings foo("), None);
    }

    #[test]
    fn continuation_indent_steps_in_after_an_opening_delimiter() {
        assert_eq!(continuation_indent("fn main() {"), "    ");
        assert_eq!(continuation_indent("    let values = ["), "        ");
    }

    #[test]
    fn continuation_indent_preserves_the_current_level() {
        assert_eq!(continuation_indent("fn main() {\n    let x = 1"), "    ");
        assert_eq!(
            continuation_indent("fn main() { // setup"),
            "    ",
            "a trailing comment should not hide the opening brace"
        );
    }
}
