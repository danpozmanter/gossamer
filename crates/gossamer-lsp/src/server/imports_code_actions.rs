fn collect_existing_imports(source: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("use ") {
            let path = rest.trim_end_matches(';').trim().to_string();
            if !path.is_empty() {
                out.push(path);
            }
        }
    }
    out
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
    action.insert("edit".to_string(), Value::Object(workspace_edit));
    // Link the action to the originating diagnostic so the
    // client groups it under the lightbulb at that location.
    let mut diag_for_link = BTreeMap::new();
    diag_for_link.insert(
        "range".to_string(),
        span_to_range(
            doc,
            diag.labels
                .iter()
                .find(|l| l.primary)
                .or_else(|| diag.labels.first())
                .map_or(suggestion.location.span, |l| l.location.span),
        ),
    );
    diag_for_link.insert(
        "severity".to_string(),
        Value::Number(severity_tag(diag.severity)),
    );
    diag_for_link.insert(
        "code".to_string(),
        Value::String(diag.code.as_str().to_string()),
    );
    diag_for_link.insert("source".to_string(), Value::String("gos".to_string()));
    diag_for_link.insert("message".to_string(), Value::String(diag.title.clone()));
    action.insert(
        "diagnostics".to_string(),
        Value::Array(vec![Value::Object(diag_for_link)]),
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

