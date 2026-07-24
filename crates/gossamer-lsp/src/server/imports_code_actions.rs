fn collect_existing_imports(doc: &DocumentAnalysis) -> Vec<String> {
    let mut out = Vec::new();
    for decl in &doc.sf.uses {
        let gossamer_ast::UseTarget::Module(target) = &decl.target else {
            continue;
        };
        let base = target
            .segments
            .iter()
            .map(|segment| segment.name.as_str())
            .collect::<Vec<_>>()
            .join("::");
        if let Some(list) = &decl.list {
            for entry in list {
                let mut path = base.clone();
                for segment in &entry.prefix {
                    path.push_str("::");
                    path.push_str(&segment.name);
                }
                path.push_str("::");
                path.push_str(&entry.name.name);
                out.push(path);
            }
        } else {
            out.push(base);
        }
    }
    out
}

fn stdlib_qualifier_is_imported(doc: &DocumentAnalysis, qualifier: &[&str]) -> bool {
    let Some(head) = qualifier.first() else {
        return false;
    };
    doc.sf.uses.iter().any(|decl| {
        if let Some(list) = &decl.list {
            return list.iter().any(|entry| {
                entry
                    .alias
                    .as_ref()
                    .map_or(entry.name.name.as_str(), |alias| alias.name.as_str())
                    == *head
            });
        }
        decl.alias
            .as_ref()
            .map(|alias| alias.name.as_str())
            .or_else(|| match &decl.target {
                gossamer_ast::UseTarget::Module(path) => {
                    path.segments.last().map(|segment| segment.name.as_str())
                }
                gossamer_ast::UseTarget::Project { module, .. } => module
                    .as_ref()
                    .and_then(|path| path.segments.last())
                    .map(|segment| segment.name.as_str()),
            })
            == Some(*head)
    })
}

fn import_completion_item(doc: &DocumentAnalysis, leaf: &str, full_path: &str) -> Value {
    let mut item = BTreeMap::new();
    item.insert("label".to_string(), Value::String(leaf.to_string()));
    item.insert("kind".to_string(), Value::Number(3.0));
    item.insert(
        "detail".to_string(),
        Value::String(format!("use {full_path}")),
    );
    item.insert(
        "documentation".to_string(),
        Value::Object({
            let mut docs = BTreeMap::new();
            docs.insert("kind".to_string(), Value::String("markdown".to_string()));
            docs.insert(
                "value".to_string(),
                Value::String(format!("Adds `use {full_path};` to the top of the file.")),
            );
            docs
        }),
    );
    let insert_offset = import_insert_offset(doc.source());
    let (line, col) = doc.offset_to_position(insert_offset);
    let mut start = BTreeMap::new();
    start.insert("line".to_string(), Value::Number(f64::from(line)));
    start.insert("character".to_string(), Value::Number(f64::from(col)));
    let end = start.clone();
    let mut range = BTreeMap::new();
    range.insert("start".to_string(), Value::Object(start));
    range.insert("end".to_string(), Value::Object(end));
    let mut edit = BTreeMap::new();
    edit.insert("range".to_string(), Value::Object(range));
    edit.insert(
        "newText".to_string(),
        Value::String(format!("use {full_path}\n")),
    );
    item.insert(
        "additionalTextEdits".to_string(),
        Value::Array(vec![Value::Object(edit)]),
    );
    Value::Object(item)
}

fn completion_with_module_import(
    doc: &DocumentAnalysis,
    item: Value,
    module_path: &str,
) -> Value {
    let Value::Object(mut fields) = item else {
        return item;
    };
    let insert_offset = import_insert_offset(doc.source());
    let (line, col) = doc.offset_to_position(insert_offset);
    let position = Value::Object(BTreeMap::from([
        ("line".to_string(), Value::Number(f64::from(line))),
        ("character".to_string(), Value::Number(f64::from(col))),
    ]));
    let range = Value::Object(BTreeMap::from([
        ("start".to_string(), position.clone()),
        ("end".to_string(), position),
    ]));
    let edit = Value::Object(BTreeMap::from([
        ("range".to_string(), range),
        (
            "newText".to_string(),
            Value::String(format!("use {module_path}\n")),
        ),
    ]));
    fields.insert("additionalTextEdits".to_string(), Value::Array(vec![edit]));
    Value::Object(fields)
}

fn import_insert_offset(source: &str) -> u32 {
    // Place new `use` after the last existing top-of-file `use` line,
    // or at byte 0 when there are none.
    let mut offset = 0usize;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("use ") || trimmed.is_empty() {
            offset += line.len();
            continue;
        }
        break;
    }
    u32::try_from(offset).unwrap_or(0)
}

/// Converts an LSP `Range` value to byte offsets in `doc`'s
/// source. Missing or malformed ranges return the full document
/// span - that matches the LSP convention where a request
/// without an explicit range asks about the entire document.
fn lsp_range_to_offsets(doc: &DocumentAnalysis, range: &Value) -> (usize, usize) {
    let source_len = doc.source().len();
    let Value::Object(map) = range else {
        return (0, source_len);
    };
    let start_offset = map
        .get("start")
        .and_then(|p| {
            let line = field_u32(p, "line")?;
            let col = field_u32(p, "character")?;
            doc.position_to_offset(line, col)
        })
        .map_or(0, |v| v as usize);
    let end_offset = map
        .get("end")
        .and_then(|p| {
            let line = field_u32(p, "line")?;
            let col = field_u32(p, "character")?;
            doc.position_to_offset(line, col)
        })
        .map_or(source_len, |v| v as usize);
    (start_offset, end_offset)
}

/// Returns whether the client-requested `only` kinds admit `kind`.
/// A parent kind such as `source` admits a more specific descendant.
fn code_action_kind_requested(params: &Value, kind: &str) -> bool {
    let Value::Array(only) = field(field(params, "context"), "only") else {
        return true;
    };
    only.iter().any(|requested| {
        let Value::String(requested) = requested else {
            return false;
        };
        kind == requested || kind.strip_prefix(requested).is_some_and(|rest| rest.starts_with('.'))
    })
}

/// Restricts quick fixes to diagnostics supplied by the client when
/// that list is non-empty. Empty context means the client wants the
/// server to derive applicable diagnostics from the requested range.
fn code_action_diagnostic_requested(
    params: &Value,
    doc: &DocumentAnalysis,
    diagnostic: &GossamerDiagnostic,
) -> bool {
    let Value::Array(requested) = field(field(params, "context"), "diagnostics") else {
        return true;
    };
    if requested.is_empty() {
        return true;
    }
    let candidate = diagnostic_to_lsp(doc, diagnostic);
    requested.iter().any(|item| {
        field(item, "code") == field(&candidate, "code")
            && field(item, "range") == field(&candidate, "range")
    })
}

fn offset_ranges_overlap(a_start: usize, a_end: usize, b_start: usize, b_end: usize) -> bool {
    if a_start == a_end {
        return b_start <= a_start && a_start <= b_end;
    }
    if b_start == b_end {
        return a_start <= b_start && b_start <= a_end;
    }
    a_start < b_end && b_start < a_end
}

fn import_to_code_action(
    doc: &DocumentAnalysis,
    uri: &str,
    diagnostic: &GossamerDiagnostic,
    path: &str,
) -> Value {
    let insertion = import_insert_offset(doc.user_source());
    let edit = build_text_edit(
        doc,
        Span::new(doc.file, insertion, insertion),
        &format!("use {path}\n"),
    );
    let mut changes = BTreeMap::new();
    changes.insert(uri.to_string(), Value::Array(vec![edit]));
    let mut workspace_edit = BTreeMap::new();
    workspace_edit.insert("changes".to_string(), Value::Object(changes));

    let mut action = BTreeMap::new();
    action.insert("title".to_string(), Value::String(format!("Import `{path}`")));
    action.insert("kind".to_string(), Value::String("quickfix".to_string()));
    action.insert("edit".to_string(), Value::Object(workspace_edit));
    action.insert(
        "diagnostics".to_string(),
        Value::Array(vec![diagnostic_to_lsp(doc, diagnostic)]),
    );
    Value::Object(action)
}

/// Builds a document-wide safe-fix action from every structured
/// compiler or lint suggestion, dropping overlapping replacements.
fn fix_all_code_action(doc: &DocumentAnalysis, uri: &str) -> Option<Value> {
    let mut suggestions: Vec<&gossamer_diagnostics::Suggestion> = doc
        .diagnostics
        .iter()
        .flat_map(|diagnostic| diagnostic.suggestions.iter())
        .filter(|suggestion| suggestion.location.span.end <= doc.user_len)
        .collect();
    suggestions.sort_by_key(|suggestion| {
        (
            suggestion.location.span.start,
            suggestion.location.span.end,
        )
    });

    let mut edits = Vec::new();
    for (index, suggestion) in suggestions.iter().enumerate() {
        let span = suggestion.location.span;
        let conflicts = suggestions.iter().enumerate().any(|(other_index, other)| {
            if index == other_index {
                return false;
            }
            let other = other.location.span;
            offset_ranges_overlap(
                other.start as usize,
                other.end as usize,
                span.start as usize,
                span.end as usize,
            )
        });
        if conflicts {
            continue;
        }
        edits.push(build_text_edit(doc, span, &suggestion.replacement));
    }
    if edits.is_empty() {
        return None;
    }

    let mut changes = BTreeMap::new();
    changes.insert(uri.to_string(), Value::Array(edits));
    let mut workspace_edit = BTreeMap::new();
    workspace_edit.insert("changes".to_string(), Value::Object(changes));
    let mut action = BTreeMap::new();
    action.insert(
        "title".to_string(),
        Value::String("Fix all auto-fixable problems".to_string()),
    );
    action.insert(
        "kind".to_string(),
        Value::String("source.fixAll.gossamer".to_string()),
    );
    action.insert("edit".to_string(), Value::Object(workspace_edit));
    Some(Value::Object(action))
}

/// Builds a single `CodeAction` of kind `quickfix` from a
/// diagnostic-attached [`gossamer_diagnostics::Suggestion`]. The
/// action's `WorkspaceEdit` replaces the suggestion's location
/// with the suggestion's `replacement` text.
fn suggestion_to_code_action(
    doc: &DocumentAnalysis,
    uri: &str,
    diag: &GossamerDiagnostic,
    suggestion: &gossamer_diagnostics::Suggestion,
) -> Value {
    let mut text_edit = BTreeMap::new();
    text_edit.insert(
        "range".to_string(),
        span_to_range(doc, suggestion.location.span),
    );
    text_edit.insert(
        "newText".to_string(),
        Value::String(suggestion.replacement.clone()),
    );

    let mut changes = BTreeMap::new();
    changes.insert(
        uri.to_string(),
        Value::Array(vec![Value::Object(text_edit)]),
    );

    let mut workspace_edit = BTreeMap::new();
    workspace_edit.insert("changes".to_string(), Value::Object(changes));

    let mut action = BTreeMap::new();
    action.insert(
        "title".to_string(),
        Value::String(suggestion.message.clone()),
    );
    action.insert("kind".to_string(), Value::String("quickfix".to_string()));
    action.insert("isPreferred".to_string(), Value::Bool(true));
    action.insert("edit".to_string(), Value::Object(workspace_edit));
    // Link the action to the originating diagnostic so the
    // client groups it under the lightbulb at that location.
    action.insert(
        "diagnostics".to_string(),
        Value::Array(vec![diagnostic_to_lsp(doc, diag)]),
    );
    Value::Object(action)
}

fn span_to_range(doc: &DocumentAnalysis, span: Span) -> Value {
    let (start_line, start_col) = doc.offset_to_position(span.start);
    let (end_line, end_col) = doc.offset_to_position(span.end);
    let mut start = BTreeMap::new();
    start.insert("line".to_string(), Value::Number(f64::from(start_line)));
    start.insert("character".to_string(), Value::Number(f64::from(start_col)));
    let mut end = BTreeMap::new();
    end.insert("line".to_string(), Value::Number(f64::from(end_line)));
    end.insert("character".to_string(), Value::Number(f64::from(end_col)));
    let mut range = BTreeMap::new();
    range.insert("start".to_string(), Value::Object(start));
    range.insert("end".to_string(), Value::Object(end));
    Value::Object(range)
}
