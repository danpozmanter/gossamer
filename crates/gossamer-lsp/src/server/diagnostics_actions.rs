/// Builds a single LSP `TextEdit` JSON value that replaces the source
/// range covered by `span` with `new_text`.
fn build_text_edit(doc: &DocumentAnalysis, span: Span, new_text: &str) -> Value {
    let mut edit = BTreeMap::new();
    edit.insert("range".to_string(), span_to_range(doc, span));
    edit.insert("newText".to_string(), Value::String(new_text.to_string()));
    Value::Object(edit)
}

/// Returns `true` when two LSP `TextEdit` values target the same
/// `range`. Used to dedup overlapping edits when the workspace
/// fan-out and the file-local fallback both produce the same span.
fn edits_overlap(a: &Value, b: &Value) -> bool {
    let (Value::Object(am), Value::Object(bm)) = (a, b) else {
        return false;
    };
    am.get("range") == bm.get("range")
}

/// Renders a `[(uri, Vec<SymbolOccurrence>)]` fan-out into a flat
/// list of LSP `Location` objects. Each location carries its own
/// `uri` so the editor can group results per file.
fn cross_file_locations(
    state: &ServerState,
    by_uri: Vec<(String, Vec<SymbolOccurrence>)>,
) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for (uri, occurrences) in by_uri {
        let Some(doc) = state.documents.get(&uri) else {
            continue;
        };
        for occ in occurrences {
            out.push(location(doc, occ.span));
        }
    }
    out
}

/// Resolves the cursor onto a [`SymbolKey`] when the locate result
/// names a workspace-tracked symbol. Local bindings always return
/// `None` so they stay file-local; field / method positions return
/// `None` when the receiver's type couldn't be resolved.
fn symbol_key_for_locate(doc: &DocumentAnalysis, loc: &Locate) -> Option<SymbolKey> {
    match loc {
        Locate::PathExpr {
            resolution: Some(Resolution::Def { def, kind }),
            ..
        }
        | Locate::TypePath {
            resolution: Some(Resolution::Def { def, kind }),
            ..
        } => {
            let info = doc.index.def(*def)?;
            let bucket = match kind {
                DefKind::Fn
                | DefKind::Struct
                | DefKind::Enum
                | DefKind::Trait
                | DefKind::Const
                | DefKind::Static
                | DefKind::TypeAlias => SymbolBucket::Item,
                DefKind::Variant => SymbolBucket::Variant,
                DefKind::Mod | DefKind::TypeParam => return None,
            };
            Some(SymbolKey {
                bucket,
                name: info.name.clone(),
            })
        }
        Locate::Field { name, owner_id, .. } => {
            let receiver_name = owner_adt_name(doc, *owner_id)?;
            // Prefer field bucket for `receiver.name` - method-only
            // names still match because the workspace lookup also
            // surfaces method-bucket entries when the field bucket
            // is empty (the test surface exercises both).
            Some(SymbolKey::field(&receiver_name, name))
        }
        _ => None,
    }
}

/// Resolves the owning expression's AST node to its ADT name when
/// the type-checker can reach a concrete struct or enum.
fn owner_adt_name(doc: &DocumentAnalysis, owner_id: gossamer_ast::NodeId) -> Option<String> {
    use gossamer_types::TyKind;
    let ty = doc.types.get(owner_id)?;
    let kind = doc.tcx.kind(ty)?;
    let def = match kind {
        TyKind::Adt { def, .. } | TyKind::Alias { def, .. } => Some(*def),
        TyKind::Ref { inner, .. } => match doc.tcx.kind(*inner)? {
            TyKind::Adt { def, .. } | TyKind::Alias { def, .. } => Some(*def),
            _ => None,
        },
        _ => None,
    }?;
    for item in &doc.sf.items {
        let Some(item_def) = doc.resolutions.definition_of(item.id) else {
            continue;
        };
        if item_def != def {
            continue;
        }
        return match &item.kind {
            gossamer_ast::ItemKind::Struct(decl) => Some(decl.name.name.clone()),
            gossamer_ast::ItemKind::Enum(decl) => Some(decl.name.name.clone()),
            _ => None,
        };
    }
    None
}

/// Heuristic: returns `true` when `offset` sits inside a string
/// literal or a fenced doctest block. Walks the source from the
/// start, toggling state on each matched delimiter. Used to gate
/// the syntactic whole-word fallback for references.
fn cursor_in_string_or_doctest(source: &str, offset: u32) -> bool {
    let cap = std::cmp::min(offset as usize, source.len());
    let bytes = source.as_bytes();
    let mut in_string = false;
    let mut in_doctest = false;
    let mut i = 0;
    while i < cap {
        let b = bytes[i];
        if !in_string && i + 3 <= bytes.len() && &bytes[i..i + 3] == b"```" {
            in_doctest = !in_doctest;
            i += 3;
            continue;
        }
        if !in_doctest && b == b'"' && !(in_string && i > 0 && bytes[i - 1] == b'\\') {
            in_string = !in_string;
        }
        i += 1;
    }
    in_string || in_doctest
}

fn severity_tag(severity: Severity) -> f64 {
    match severity {
        Severity::Error => 1.0,
        Severity::Warning => 2.0,
        Severity::Note => 3.0,
        Severity::Help => 4.0,
    }
}

fn diagnostic_to_lsp(doc: &DocumentAnalysis, diag: &GossamerDiagnostic) -> Value {
    let span = diag
        .labels
        .iter()
        .find(|l| l.primary)
        .or_else(|| diag.labels.first())
        .map_or(Span::new(doc.file, 0, 0), |l| l.location.span);
    let mut entry = BTreeMap::new();
    entry.insert("range".to_string(), span_to_range(doc, span));
    entry.insert(
        "severity".to_string(),
        Value::Number(severity_tag(diag.severity)),
    );
    entry.insert(
        "code".to_string(),
        Value::String(diag.code.as_str().to_string()),
    );
    entry.insert("source".to_string(), Value::String("gos".to_string()));
    entry.insert("message".to_string(), Value::String(diag.title.clone()));
    Value::Object(entry)
}

const KEYWORDS: &[&str] = &[
    "fn", "let", "mut", "if", "else", "match", "while", "loop", "for", "in", "return", "break",
    "continue", "struct", "enum", "trait", "impl", "pub", "use", "mod", "const", "static", "true",
    "false", "go", "select", "defer", "where", "as",
];

const BUILTIN_COMPLETIONS: &[&str] = &[
    "println",
    "print",
    "eprintln",
    "eprint",
    "format",
    "panic",
    "Some",
    "None",
    "Ok",
    "Err",
    "len",
    "push",
    "to_string",
    "clone",
    "unwrap",
    "unwrap_or",
    "is_some",
    "is_none",
    "is_ok",
    "is_err",
    "map",
    "spawn",
    "channel",
];

