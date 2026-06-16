# `std::html::template`

Status: experimental

Context-aware HTML templates with auto-escape (text/attr/URL/JS). The context classifier is heuristic — sound for typical server-rendered responses but NOT a content-security-policy substitute; sanitize untrusted HTML fragments with a dedicated sanitizer.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Template` | type | Compiled HTML template. |
| `parse` | fn | Parses a template string. |
| `render` | fn | Renders a template with the supplied data context. |
| `render_json` | fn | render_json(source, json_data) -> Result<String, Error>: renders a context-aware HTML template against a JSON data context. Stateless and wired bit-identically across every tier. |

