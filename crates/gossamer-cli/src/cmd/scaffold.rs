//! `gos init` and `gos new` — project scaffolding plus the inline
//! source / manifest / README templates each `--template` choice
//! emits.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};

/// `gos init ID` — drops a `project.toml` (and a starter
/// `src/main.gos` when neither it nor `src/lib.gos` exists) into
/// the current directory.
pub(crate) fn init(id: &str) -> Result<()> {
    let project =
        gossamer_pkg::ProjectId::parse(id).with_context(|| format!("invalid id `{id}`"))?;
    let manifest_path = PathBuf::from("project.toml");
    if manifest_path.exists() {
        return Err(anyhow!("`project.toml` already exists"));
    }
    let manifest =
        gossamer_pkg::render_initial_manifest(&project, gossamer_pkg::Version::new(0, 1, 0));
    fs::write(&manifest_path, &manifest)
        .with_context(|| format!("writing {}", manifest_path.display()))?;
    let src_dir = PathBuf::from("src");
    let main_gos = src_dir.join("main.gos");
    let lib_gos = src_dir.join("lib.gos");
    let scaffolded = if !main_gos.exists() && !lib_gos.exists() {
        fs::create_dir_all(&src_dir).with_context(|| format!("creating {}", src_dir.display()))?;
        let body = gossamer_pkg::render_main_source(&project);
        fs::write(&main_gos, body).with_context(|| format!("writing {}", main_gos.display()))?;
        true
    } else {
        false
    };
    if scaffolded {
        println!("init: created project.toml + src/main.gos for {project}");
        println!("hint: try `gos run` or `gos test`");
    } else {
        println!("init: created project.toml for {project}");
    }
    Ok(())
}

/// `gos new ID --path P --template T` — scaffolds a fresh project
/// directory according to the chosen template (`bin`, `lib`,
/// `service`, or `workspace`).
pub(crate) fn new(id: &str, path: Option<PathBuf>, template: &str) -> Result<()> {
    let project =
        gossamer_pkg::ProjectId::parse(id).with_context(|| format!("invalid id `{id}`"))?;
    let dir = path.unwrap_or_else(|| PathBuf::from(project.tail()));
    if dir.exists() {
        return Err(anyhow!("{} already exists", dir.display()));
    }
    let manifest =
        gossamer_pkg::render_initial_manifest(&project, gossamer_pkg::Version::new(0, 1, 0));
    match template {
        "bin" => {
            fs::create_dir_all(dir.join("src"))
                .with_context(|| format!("creating {}", dir.display()))?;
            fs::write(dir.join("project.toml"), &manifest)?;
            fs::write(
                dir.join("src/main.gos"),
                gossamer_pkg::render_main_source(&project),
            )?;
        }
        "lib" => {
            fs::create_dir_all(dir.join("src"))
                .with_context(|| format!("creating {}", dir.display()))?;
            fs::write(dir.join("project.toml"), &manifest)?;
            fs::write(dir.join("src/lib.gos"), lib_template_source(&project))?;
            fs::write(dir.join("src/lib_test.gos"), lib_template_test_source())?;
        }
        "service" => {
            fs::create_dir_all(dir.join("src"))
                .with_context(|| format!("creating {}", dir.display()))?;
            fs::write(dir.join("project.toml"), &manifest)?;
            fs::write(dir.join("src/main.gos"), service_template_source(&project))?;
        }
        "workspace" => {
            fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            fs::write(
                dir.join("project.toml"),
                workspace_template_manifest(&project),
            )?;
            fs::write(dir.join("README.md"), workspace_template_readme(&project))?;
        }
        other => {
            return Err(anyhow!(
                "unknown template `{other}` — expected bin, lib, service, or workspace"
            ));
        }
    }
    println!(
        "new: scaffolded {} ({} template) at {}",
        project,
        template,
        dir.display()
    );
    Ok(())
}

/// Returns the seed `src/lib.gos` for `--template lib`.
fn lib_template_source(project: &gossamer_pkg::ProjectId) -> String {
    format!(
        "//! {project} — library crate.\n\
         //!\n\
         //! Replace this scaffolding with the real API before\n\
         //! publishing.\n\
         \n\
         /// Returns a greeting addressed to `name`.\n\
         pub fn greet(name: &str) -> String {{\n\
         \x20\x20\x20\x20\"hello, \".to_string() + name\n\
         }}\n",
    )
}

/// Returns the seed `src/main.gos` for `--template service`.
fn service_template_source(project: &gossamer_pkg::ProjectId) -> String {
    format!(
        "//! {project} — HTTP service entry point.\n\
         //!\n\
         //! Listens on 0.0.0.0:8080 and answers `/health` with a 200.\n\
         //! Replace the match arms with your real routes before shipping.\n\
         \n\
         use std::http\n\
         \n\
         struct App {{ }}\n\
         \n\
         impl http::Handler for App {{\n\
         \x20\x20\x20\x20fn serve(&self, request: http::Request) -> Result<http::Response, http::Error> {{\n\
         \x20\x20\x20\x20\x20\x20\x20\x20match request.path() {{\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\"/health\" => Ok(http::Response::text(200, \"ok\".to_string())),\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20_ => Ok(http::Response::text(404, \"not found\".to_string())),\n\
         \x20\x20\x20\x20\x20\x20\x20\x20}}\n\
         \x20\x20\x20\x20}}\n\
         }}\n\
         \n\
         fn main() -> Result<(), http::Error> {{\n\
         \x20\x20\x20\x20let app = App {{ }}\n\
         \x20\x20\x20\x20println!(\"listening on 0.0.0.0:8080\")\n\
         \x20\x20\x20\x20http::serve(\"0.0.0.0:8080\".to_string(), app)\n\
         }}\n",
    )
}

/// Returns the seed test fixture for `--template lib`.
fn lib_template_test_source() -> String {
    "//! Smoke tests for the library crate.\n\
     \n\
     use std::testing\n\
     \n\
     #[test]\n\
     fn greet_includes_name() {\n\
     \x20\x20\x20\x20testing::check_eq(&greet(\"gossamer\"), &\"hello, gossamer\".to_string(), \"greet round-trips\").expect(\"mismatch\")\n\
     }\n"
        .to_string()
}

/// Returns the `project.toml` contents for `--template workspace`.
fn workspace_template_manifest(project: &gossamer_pkg::ProjectId) -> String {
    format!(
        "[package]\n\
         id = \"{project}\"\n\
         version = \"0.1.0\"\n\
         \n\
         [workspace]\n\
         members = []\n",
    )
}

/// Returns a README.md stub for `--template workspace`.
fn workspace_template_readme(project: &gossamer_pkg::ProjectId) -> String {
    format!(
        "# {project}\n\
         \n\
         A Gossamer workspace. Add members under `members/` and list\n\
         their ids under `[workspace.members]` in `project.toml`.\n",
    )
}
