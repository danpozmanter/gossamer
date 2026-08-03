//! End-to-end coverage for top-level statements (implicit `fn main`).

use std::path::{Path, PathBuf};
use std::process::Command;

fn gos() -> Command {
    Command::new(env!("CARGO_BIN_EXE_gos"))
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gos-tls-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
fn run_hello_without_main() {
    let dir = scratch("hello");
    let path = write_file(&dir, "hello.gos", "println!(\"Hello World\")\n");
    let out = gos().arg("run").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "Hello World\n");
}

#[test]
fn run_question_propagation_without_main() {
    let dir = scratch("question");
    let src = "use std::strconv\nlet n = strconv::parse_i64(&\"41\")?\nprintln!(\"{}\", n + 1)\n";
    let path = write_file(&dir, "q.gos", src);
    let out = gos().arg("run").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "42\n");
}

#[test]
fn mixing_with_explicit_main_is_rejected() {
    let dir = scratch("mix");
    let path = write_file(&dir, "mix.gos", "println!(\"hi\")\nfn main() { }\n");
    let out = gos().arg("check").arg(&path).output().unwrap();
    assert!(!out.status.success(), "mixing should fail to check");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("top-level statements") && stderr.contains("fn main"),
        "stderr: {stderr}"
    );
}

#[test]
fn manifest_entry_selects_top_level_file() {
    let dir = scratch("entry");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    write_file(
        &dir,
        "project.toml",
        "[project]\nid = \"example.com/app\"\nversion = \"0.1.0\"\nentry = \"src/app.gos\"\n",
    );
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src").join("app.gos"), "println!(\"from app\")\n").unwrap();
    let out = gos()
        .arg("run")
        .arg(".")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "from app\n");
}
