//! Scaffolders behind `gos init` / `gos new`.

#![forbid(unsafe_code)]

use crate::id::ProjectId;
use crate::version::Version;

/// Renders a starter `project.toml` for the given identifier and
/// initial version.
#[must_use]
pub fn render_initial_manifest(id: &ProjectId, version: Version) -> String {
    format!("[project]\nid = \"{id}\"\nversion = \"{version}\"\n\n[dependencies]\n")
}

/// Renders a starter `src/main.gos` body printing a greeting.
#[must_use]
pub fn render_main_source(id: &ProjectId) -> String {
    format!(
        "use fmt\n\nfn main() {{\n    fmt::println(\"hello from {tail}\")\n}}\n",
        tail = id.tail()
    )
}
