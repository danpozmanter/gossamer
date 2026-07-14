# `std::html`

Status: experimental

HTML text escaping and unescaping.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/html.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`escape`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/html.rs) | `fn escape(text: String) -> String` | Escapes HTML metacharacters (&, <, >, ", '). |
| [`render_json`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/html.rs) | `fn render_json(template: String, data: json::Value) -> Result<String, errors::Error>` | render_json(source, json_data) -> Result<String, Error>: renders a context-aware HTML template against a JSON data context. Stateless and wired bit-identically across every tier. |
| [`unescape`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/html.rs) | `fn unescape(text: String) -> String` | Resolves HTML entities back to their characters. |
