//! Templates for the per-project Cargo runner that statically
//! links the user's `[rust-bindings]` into a `gos`-equivalent
//! binary.
//!
//! The renderer is hand-rolled (no external templating crate) and
//! supports the subset of placeholders we need:
//!
//! - `{{ key }}` — substituted with the value of `key` on the
//!   render input.
//! - `{{#each bindings}} … {{/each}}` — repeated once per binding,
//!   with `{{ this.<field> }}` resolving against the entry.
//!
//! The two binding-entry fields the templates use are:
//!
//! - `cargo_dep_line` — a fully rendered Cargo dep line such as
//!   `echo-binding = { path = "..." }`.
//! - `crate_name_ident` — the binding's Cargo package name with
//!   hyphens replaced by underscores so it forms a valid Rust
//!   `extern crate` ident (e.g. `echo_binding`).

#![deny(missing_docs)]

use std::path::Path;

/// Raw template text for the runner's `Cargo.toml`.
pub const CARGO_TOML: &str = include_str!("../templates/Cargo.toml.tmpl");

/// Raw template text for the runner's `main.rs`.
pub const MAIN_RS: &str = include_str!("../templates/main.rs.tmpl");

/// Raw template text for the signature-dumping bin's `sigs_dump.rs`.
pub const SIGS_DUMP_RS: &str = include_str!("../templates/sigs_dump.rs.tmpl");

/// Raw template text for the compiled-mode `Cargo.toml` (staticlib).
pub const STATICLIB_CARGO_TOML: &str = include_str!("../templates/staticlib_Cargo.toml.tmpl");

/// Raw template text for the compiled-mode `lib.rs` (staticlib).
pub const STATICLIB_LIB_RS: &str = include_str!("../templates/staticlib_lib.rs.tmpl");

/// Render profile selecting the cargo build profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// `cargo build` (no `--release`).
    Debug,
    /// `cargo build --release`.
    Release,
}

impl Profile {
    /// Returns the cargo profile directory name (`debug` / `release`).
    #[must_use]
    pub fn dir(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }

    /// Returns `--release` flag string when applicable.
    #[must_use]
    pub fn cargo_flag(self) -> Option<&'static str> {
        match self {
            Self::Debug => None,
            Self::Release => Some("--release"),
        }
    }
}

/// One binding entry as supplied to the renderer.
#[derive(Debug, Clone)]
pub struct BindingEntry {
    /// Cargo package name as it appears under `[rust-bindings]`.
    pub crate_name: String,
    /// Cargo dep line, e.g. `foo = { path = "/abs/path" }`.
    pub cargo_dep_line: String,
    /// Cargo features requested for this binding.
    pub features: Vec<String>,
}

impl BindingEntry {
    /// Cargo package name converted to a Rust ident
    /// (`-` → `_`).
    #[must_use]
    pub fn crate_name_ident(&self) -> String {
        self.crate_name.replace('-', "_")
    }
}

/// Inputs for rendering the runner's `Cargo.toml` / `main.rs`.
#[derive(Debug, Clone)]
pub struct RenderInput<'a> {
    /// Identifier of the user's project (sanitised for Cargo).
    pub project_id: &'a str,
    /// Hex prefix of the manifest fingerprint (12+ chars).
    pub fingerprint_hex: &'a str,
    /// Absolute path to the gossamer source tree (used for path
    /// dependencies in the rendered Cargo.toml).
    pub gossamer_root: &'a Path,
    /// Bindings to include.
    pub bindings: &'a [BindingEntry],
    /// Cargo profile to build with.
    pub profile: Profile,
}

/// Renders the runner's `Cargo.toml`.
#[must_use]
pub fn render_cargo_toml(input: &RenderInput) -> String {
    render(CARGO_TOML, input)
}

/// Renders the runner's `main.rs`.
#[must_use]
pub fn render_main_rs(input: &RenderInput) -> String {
    render(MAIN_RS, input)
}

/// Renders the signature-dumping bin's `sigs_dump.rs`.
#[must_use]
pub fn render_sigs_dump_rs(input: &RenderInput) -> String {
    render(SIGS_DUMP_RS, input)
}

/// Renders the compiled-mode staticlib's `Cargo.toml`.
#[must_use]
pub fn render_staticlib_cargo_toml(input: &RenderInput) -> String {
    render(STATICLIB_CARGO_TOML, input)
}

/// Renders the compiled-mode staticlib's `lib.rs`.
#[must_use]
pub fn render_staticlib_lib_rs(input: &RenderInput) -> String {
    render(STATICLIB_LIB_RS, input)
}

fn render(template: &str, input: &RenderInput) -> String {
    let mut out = String::with_capacity(template.len() * 2);
    let mut rest = template;
    while !rest.is_empty() {
        if let Some(idx) = rest.find("{{#each bindings}}") {
            out.push_str(&rest[..idx]);
            let after_open = &rest[idx + "{{#each bindings}}".len()..];
            let close = after_open
                .find("{{/each}}")
                .expect("each block missing closing tag");
            let body = &after_open[..close];
            // Strip a leading newline so the block doesn't
            // produce a stray blank line in the output.
            let body = body.strip_prefix('\n').unwrap_or(body);
            for binding in input.bindings {
                out.push_str(&render_each(body, binding));
            }
            rest = &after_open[close + "{{/each}}".len()..];
            // Strip a trailing newline so the closing tag's
            // newline doesn't double-up.
            rest = rest.strip_prefix('\n').unwrap_or(rest);
        } else {
            out.push_str(rest);
            break;
        }
    }
    substitute_top(&out, input)
}

fn render_each(body: &str, binding: &BindingEntry) -> String {
    body.replace("{{ this.cargo_dep_line }}", &binding.cargo_dep_line)
        .replace("{{ this.crate_name }}", &binding.crate_name)
        .replace("{{ this.crate_name_ident }}", &binding.crate_name_ident())
}

fn substitute_top(s: &str, input: &RenderInput) -> String {
    let gossamer_root = input.gossamer_root.display().to_string();
    s.replace("{{ project_id }}", input.project_id)
        .replace("{{ fingerprint_hex }}", input.fingerprint_hex)
        .replace("{{ gossamer_root }}", &gossamer_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_input(profile: Profile) -> (PathBuf, Vec<BindingEntry>, &'static str) {
        let root = PathBuf::from("/abs/gossamer");
        let bindings = vec![
            BindingEntry {
                crate_name: "echo-binding".to_string(),
                cargo_dep_line: r#"echo-binding = { path = "/abs/echo" }"#.to_string(),
                features: vec![],
            },
            BindingEntry {
                crate_name: "tuigoose".to_string(),
                cargo_dep_line: r#"tuigoose = { path = "/abs/tuigoose" }"#.to_string(),
                features: vec![],
            },
        ];
        (
            root,
            bindings,
            match profile {
                Profile::Debug => "debug",
                Profile::Release => "release",
            },
        )
    }

    #[test]
    fn render_cargo_toml_is_byte_stable_and_parses() {
        let (root, bindings, _) = sample_input(Profile::Debug);
        let input = RenderInput {
            project_id: "example.com/proj",
            fingerprint_hex: "deadbeefcafe",
            gossamer_root: &root,
            bindings: &bindings,
            profile: Profile::Debug,
        };
        let out = render_cargo_toml(&input);
        // Byte-stability: rendering twice produces identical output.
        let again = render_cargo_toml(&input);
        assert_eq!(out, again);
        assert!(out.contains("name = \"gos-runner-deadbeefcafe\""));
        assert!(out.contains(r#"echo-binding = { path = "/abs/echo" }"#));
        assert!(out.contains(r#"tuigoose = { path = "/abs/tuigoose" }"#));
        // Sanity: parse as TOML.
        let _: toml::Value = toml::from_str(&out).expect("rendered Cargo.toml is valid TOML");
    }

    #[test]
    fn render_main_rs_is_byte_stable_and_parses() {
        let (root, bindings, _) = sample_input(Profile::Debug);
        let input = RenderInput {
            project_id: "example.com/proj",
            fingerprint_hex: "deadbeefcafe",
            gossamer_root: &root,
            bindings: &bindings,
            profile: Profile::Debug,
        };
        let out = render_main_rs(&input);
        let again = render_main_rs(&input);
        assert_eq!(out, again);
        assert!(out.contains("extern crate echo_binding;"));
        assert!(out.contains("extern crate tuigoose;"));
        assert!(out.contains("echo_binding::__bindings_force_link()"));
        assert!(syn::parse_file(&out).is_ok());
    }

    #[test]
    fn render_sigs_dump_rs_parses() {
        let (root, bindings, _) = sample_input(Profile::Debug);
        let input = RenderInput {
            project_id: "example.com/proj",
            fingerprint_hex: "deadbeefcafe",
            gossamer_root: &root,
            bindings: &bindings,
            profile: Profile::Debug,
        };
        let out = render_sigs_dump_rs(&input);
        assert!(out.contains("extern crate echo_binding;"));
        assert!(syn::parse_file(&out).is_ok());
    }

    #[test]
    fn render_staticlib_files_parse() {
        let (root, bindings, _) = sample_input(Profile::Release);
        let input = RenderInput {
            project_id: "example.com/proj",
            fingerprint_hex: "deadbeefcafe",
            gossamer_root: &root,
            bindings: &bindings,
            profile: Profile::Release,
        };
        let cargo = render_staticlib_cargo_toml(&input);
        let _: toml::Value = toml::from_str(&cargo).expect("staticlib Cargo.toml parses");
        let lib = render_staticlib_lib_rs(&input);
        assert!(syn::parse_file(&lib).is_ok());
        assert!(lib.contains("gos_static_install_bindings"));
    }

    #[test]
    fn empty_bindings_render_clean() {
        let root = PathBuf::from("/abs");
        let empty: Vec<BindingEntry> = Vec::new();
        let input = RenderInput {
            project_id: "p",
            fingerprint_hex: "abcdef012345",
            gossamer_root: &root,
            bindings: &empty,
            profile: Profile::Debug,
        };
        let out = render_cargo_toml(&input);
        assert!(toml::from_str::<toml::Value>(&out).is_ok());
        let main = render_main_rs(&input);
        assert!(syn::parse_file(&main).is_ok());
    }
}
