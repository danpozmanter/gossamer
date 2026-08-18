//! Scaffolders behind `gos init` / `gos new`.

#![forbid(unsafe_code)]

use crate::id::ProjectId;
use crate::version::Version;

/// Renders a starter `project.toml` for the given identifier and
/// initial version.
///
/// `gossamer-version` states the exact toolchain the project is written
/// against, spelled the way the release tag is.
#[must_use]
pub fn render_initial_manifest(id: &ProjectId, version: Version) -> String {
    let toolchain = crate::toolchain_version();
    format!(
        "[project]\nid = \"{id}\"\nversion = \"{version}\"\n\
         gossamer-version = \"v{toolchain}\"\n\n[dependencies]\n"
    )
}

/// Renders a starter `src/main.gos` body printing a greeting.
///
/// Uses the builtin `println!` macro — one of the six format macros
/// always in scope, no `use` required. The older scaffold spelled this
/// `fmt::println`, but `std::fmt` exposes only the `Display`/`Debug`
/// traits and formatting helpers, not a `println` function, so the
/// scaffolded program failed to run with GX0002.
#[must_use]
pub fn render_main_source(id: &ProjectId) -> String {
    format!(
        "fn main() {{\n    println!(\"hello from {tail}\")\n}}\n",
        tail = id.tail()
    )
}
