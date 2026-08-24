# `std::html::template`

Context-aware HTML templates with auto-escape (text/attr/URL/JS). The context classifier is heuristic - sound for typical server-rendered responses but NOT a content-security-policy substitute; sanitize untrusted HTML fragments with a dedicated sanitizer.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/html.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`render_json`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/html.rs) | `fn render_json(template: String, data: json::Value) -> Result<String, errors::Error>` | render_json(source, json_data) -> Result<String, Error>: renders a context-aware HTML template against a JSON data context. Stateless and wired bit-identically across every tier. |
