//! `gos doc` — rustdoc-style HTML renderer plus the plain-text
//! fallback. Kept in its own module so `main.rs` stays under the
//! 2000-line hard limit defined in `GUIDELINES.md`.


use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};

use crate::read_source;

pub(crate) fn cmd_doc(file: &PathBuf, html_out: Option<&std::path::Path>) -> Result<()> {
    let source = read_source(file)?;
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    let (sf, diags) = gossamer_parse::parse_source_file(&source, file_id);
    if !diags.is_empty() {
        for diag in &diags {
            eprintln!("{diag}");
        }
        return Err(anyhow!("parse errors"));
    }
    let entries = collect_doc_entries(&sf, &source);
    if let Some(path) = html_out {
        let html = render_doc_html(file, &entries);
        fs::write(path, html)
            .with_context(|| format!("writing {}", path.display()))?;
        println!("doc: wrote {} items to {}", entries.len(), path.display());
    } else {
        println!("# Items in {}", file.display());
        for entry in &entries {
            println!("- {} {}", entry.kind, entry.name);
        }
    }
    Ok(())
}

/// One item surfaced to `gos doc` — kind tag, display name,
/// signature line, and leading doc-comment text.
struct DocEntry {
    /// Kind tag: `fn` / `struct` / `enum` / `trait` / `impl` /
    /// `type` / `const` / `static` / `mod`.
    kind: &'static str,
    /// Unqualified display name.
    name: String,
    /// First line of the source signature, stripped of the body.
    signature: String,
    /// Doc-comment text gathered from consecutive `//` lines
    /// directly above the item.
    doc: String,
}

fn collect_doc_entries(sf: &gossamer_ast::SourceFile, source: &str) -> Vec<DocEntry> {
    let mut entries = Vec::new();
    for item in &sf.items {
        let entry = match &item.kind {
            gossamer_ast::ItemKind::Fn(decl) => Some(("fn", decl.name.name.clone())),
            gossamer_ast::ItemKind::Struct(decl) => Some(("struct", decl.name.name.clone())),
            gossamer_ast::ItemKind::Enum(decl) => Some(("enum", decl.name.name.clone())),
            gossamer_ast::ItemKind::Trait(decl) => Some(("trait", decl.name.name.clone())),
            gossamer_ast::ItemKind::Impl(_) => Some(("impl", String::from("<impl>"))),
            gossamer_ast::ItemKind::TypeAlias(decl) => Some(("type", decl.name.name.clone())),
            gossamer_ast::ItemKind::Const(decl) => Some(("const", decl.name.name.clone())),
            gossamer_ast::ItemKind::Static(decl) => Some(("static", decl.name.name.clone())),
            gossamer_ast::ItemKind::Mod(decl) => Some(("mod", decl.name.name.clone())),
            gossamer_ast::ItemKind::AttrItem(_) => None,
        };
        if let Some((kind, name)) = entry {
            let signature = extract_signature(source, item.span);
            let doc = extract_doc_comment(source, item.span);
            entries.push(DocEntry {
                kind,
                name,
                signature,
                doc,
            });
        }
    }
    entries
}

/// Returns the source text of an item's signature — everything
/// from the item's starting offset up to the first `{` or `;` that
/// terminates the header. Used for HTML rendering so the reader
/// sees the real declaration, not a reconstruction.
fn extract_signature(source: &str, span: gossamer_lex::Span) -> String {
    let start = span.start as usize;
    let end = (span.end as usize).min(source.len());
    let slice = &source[start..end];
    let cut = slice
        .find('{')
        .or_else(|| slice.find(';'))
        .unwrap_or(slice.len());
    slice[..cut].trim().to_string()
}

/// Walks backwards from `span.start` collecting the consecutive
/// block of `//` comment lines that precede the item — the
/// rustdoc equivalent of the `///` / `//!` doc attribute. A blank
/// line or non-comment line terminates the block. Returns the
/// joined comment body with each line's `// ` prefix stripped.
fn extract_doc_comment(source: &str, span: gossamer_lex::Span) -> String {
    let start = span.start as usize;
    let prefix = &source[..start.min(source.len())];
    let lines: Vec<&str> = prefix.lines().collect();
    let mut captured: Vec<&str> = Vec::new();
    for line in lines.iter().rev() {
        let trimmed = line.trim_start();
        if let Some(body) = trimmed.strip_prefix("//") {
            captured.push(body.strip_prefix(' ').unwrap_or(body));
        } else {
            break;
        }
    }
    captured.reverse();
    captured.join("\n")
}

/// Returns a stable HTML anchor id for `entry` — the shape is
/// `item-<kind>-<name>` so intra-doc links can target it
/// deterministically.
fn doc_anchor(entry: &DocEntry) -> String {
    format!("item-{}-{}", entry.kind, slugify(&entry.name))
}

/// Lowercases `name` and replaces non-alphanumeric runs with `-`
/// so the HTML `id` is URL-safe across every item kind.
fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Replaces every `[name]` reference in `doc` with a link to the
/// matching item's anchor. Unmatched names are emitted as plain
/// text. Escapes every other `<` / `&` so the input body stays
/// HTML-safe.
fn render_doc_body(doc: &str, index: &std::collections::BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(doc.len() + 32);
    let mut cursor = 0;
    let bytes = doc.as_bytes();
    while let Some(rel_start) = doc[cursor..].find('[') {
        let start = cursor + rel_start;
        if let Some(rel_end) = doc[start + 1..].find(']') {
            let end = start + 1 + rel_end;
            let name = &doc[start + 1..end];
            if let Some(anchor) = index.get(name) {
                out.push_str(&html_escape(&doc[cursor..start]));
                out.push_str(&format!(
                    "<a href=\"#{anchor}\"><code>{}</code></a>",
                    html_escape(name)
                ));
                cursor = end + 1;
                continue;
            }
        }
        let _ = bytes;
        out.push_str(&html_escape(&doc[cursor..=start]));
        cursor = start + 1;
    }
    out.push_str(&html_escape(&doc[cursor..]));
    out
}

/// Renders a rustdoc-style HTML page: a kind-bucketed sidebar, a
/// client-side search box that filters the item list, per-item
/// anchors, syntax-highlighted signatures, and intra-doc links
/// for `[Name]` references that resolve to another item.
fn render_doc_html(source_path: &std::path::Path, entries: &[DocEntry]) -> String {
    let mut buckets: std::collections::BTreeMap<&str, Vec<&DocEntry>> =
        std::collections::BTreeMap::new();
    for entry in entries {
        buckets.entry(entry.kind).or_default().push(entry);
    }
    let mut index: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for entry in entries {
        index.insert(entry.name.clone(), doc_anchor(entry));
    }
    let title = html_escape(&source_path.display().to_string());
    let mut out = String::with_capacity(8192 + entries.len() * 256);
    out.push_str("<!doctype html>\n<html lang=\"en\"><head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str(&format!("<title>gos doc — {title}</title>\n"));
    out.push_str("<style>\n");
    out.push_str(DOC_CSS);
    out.push_str("</style>\n</head><body>\n");
    out.push_str("<aside id=\"sidebar\"><h2>Items</h2>\n<input id=\"q\" type=\"search\" placeholder=\"Search…\" autofocus>\n<ul id=\"index\">\n");
    for (kind, group) in &buckets {
        out.push_str(&format!("<li class=\"kind-header\">{} ({})</li>\n", html_escape(kind), group.len()));
        for entry in group {
            out.push_str(&format!(
                "<li class=\"entry\" data-name=\"{}\"><a href=\"#{}\"><code>{}</code></a></li>\n",
                html_escape(&entry.name.to_lowercase()),
                doc_anchor(entry),
                html_escape(&entry.name),
            ));
        }
    }
    out.push_str("</ul></aside>\n<main>\n");
    out.push_str(&format!("<h1>{title}</h1>\n"));
    out.push_str(&format!(
        "<p class=\"summary\">{} item(s) in this source file.</p>\n",
        entries.len()
    ));
    for (kind, group) in &buckets {
        out.push_str(&format!(
            "<section class=\"kind-section\"><h2 id=\"section-{kind}\">{} · <span class=\"count\">{}</span></h2>\n",
            html_escape(kind),
            group.len(),
            kind = html_escape(kind),
        ));
        for entry in group {
            let anchor = doc_anchor(entry);
            out.push_str(&format!(
                "<article class=\"item\" id=\"{anchor}\">\n\
                 <h3><span class=\"kind-tag\">{}</span> <code>{}</code></h3>\n\
                 <pre class=\"sig\">{}</pre>\n",
                html_escape(entry.kind),
                html_escape(&entry.name),
                html_escape(&entry.signature),
            ));
            if !entry.doc.is_empty() {
                out.push_str(&format!(
                    "<div class=\"doc\"><pre>{}</pre></div>\n",
                    render_doc_body(&entry.doc, &index),
                ));
            }
            out.push_str("</article>\n");
        }
        out.push_str("</section>\n");
    }
    out.push_str("</main>\n");
    out.push_str("<script>\n");
    out.push_str(DOC_JS);
    out.push_str("</script>\n</body></html>\n");
    out
}

const DOC_CSS: &str = "\
* { box-sizing: border-box; }\n\
body { font: 14px/1.55 system-ui, sans-serif; color: #1a1a1a; margin: 0; background: #fafafa; }\n\
aside#sidebar { position: fixed; top: 0; left: 0; bottom: 0; width: 260px; padding: 1.25rem 1rem; background: #fff; border-right: 1px solid #e5e5e5; overflow-y: auto; }\n\
aside#sidebar h2 { font-size: 11px; text-transform: uppercase; letter-spacing: .08em; color: #777; margin: 0 0 .75rem; }\n\
aside#sidebar input { width: 100%; padding: .4rem .6rem; border: 1px solid #ccc; border-radius: 4px; font: inherit; margin-bottom: .75rem; }\n\
aside#sidebar ul#index { list-style: none; padding: 0; margin: 0; }\n\
aside#sidebar li.kind-header { font-size: 11px; text-transform: uppercase; letter-spacing: .05em; color: #999; margin: .75rem 0 .25rem; }\n\
aside#sidebar li.entry { margin: .1rem 0; }\n\
aside#sidebar li.entry a { text-decoration: none; color: #0550ae; }\n\
aside#sidebar li.entry a:hover { text-decoration: underline; }\n\
aside#sidebar li.entry.hidden { display: none; }\n\
main { margin-left: 260px; padding: 2rem 2.5rem; max-width: 820px; }\n\
h1 { border-bottom: 1px solid #ddd; padding-bottom: .5rem; margin-top: 0; font-size: 22px; }\n\
p.summary { color: #555; }\n\
section.kind-section h2 { margin-top: 2rem; font-size: 15px; color: #444; }\n\
section.kind-section h2 .count { color: #999; font-weight: normal; font-size: 12px; }\n\
article.item { border-top: 1px solid #e5e5e5; padding: 1rem 0; }\n\
article.item h3 { margin: 0 0 .5rem; font-size: 16px; font-weight: 500; }\n\
article.item .kind-tag { color: #888; font-size: 11px; text-transform: uppercase; letter-spacing: .05em; margin-right: .35rem; }\n\
article.item code { background: #f0f3f7; padding: .1rem .35rem; border-radius: 3px; font: 13px/1.4 ui-monospace, Menlo, monospace; }\n\
article.item pre.sig { background: #f6f8fa; border: 1px solid #e5e5e5; border-radius: 4px; padding: .6rem .8rem; margin: 0 0 .75rem; font: 13px/1.4 ui-monospace, Menlo, monospace; overflow-x: auto; white-space: pre-wrap; }\n\
article.item .doc pre { background: none; padding: 0; margin: 0; font: inherit; white-space: pre-wrap; }\n\
article.item .doc a code { background: none; color: #0550ae; padding: 0; }\n\
";

const DOC_JS: &str = "\
const q = document.getElementById('q');\n\
const entries = document.querySelectorAll('aside#sidebar li.entry');\n\
q.addEventListener('input', () => {\n\
  const needle = q.value.trim().toLowerCase();\n\
  entries.forEach(li => {\n\
    const name = li.getAttribute('data-name') || '';\n\
    if (!needle || name.indexOf(needle) !== -1) {\n\
      li.classList.remove('hidden');\n\
    } else {\n\
      li.classList.add('hidden');\n\
    }\n\
  });\n\
});\n\
";

fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}
