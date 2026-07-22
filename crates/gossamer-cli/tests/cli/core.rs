// End-to-end CLI tests.
// Shells out to the `gos` binary Cargo produces for this crate and
// asserts behaviour for `parse`, `check`, `run`, `build`, plus
// cross-compilation via `--target`.


use std::env;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn gos_bin() -> PathBuf {
    // CARGO_BIN_EXE_<name> is set by cargo when running tests.
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

fn write_fixture(name: &str, source: &str) -> PathBuf {
    let mut path = env::temp_dir();
    path.push(format!("gossamer-cli-{}-{}.gos", name, std::process::id()));
    std::fs::write(&path, source).expect("write fixture");
    path
}

fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .join("examples")
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

#[test]
fn version_flag_prints_package_version() {
    let out = Command::new(gos_bin())
        .arg("--version")
        .output()
        .expect("spawn --version");
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    assert!(stdout.contains("gos"));
}

#[test]
fn cache_status_uses_human_readable_sizes_by_default() {
    let root = env::temp_dir().join(format!("gossamer-cache-status-{}", std::process::id()));
    let frontend = root.join("frontend");
    let project = root.join("project");
    std::fs::create_dir_all(&frontend).expect("create frontend cache");
    std::fs::create_dir_all(&project).expect("create project");
    std::fs::write(frontend.join("entry"), vec![0_u8; 1536]).expect("write cache entry");

    let out = Command::new(gos_bin())
        .arg("cache")
        .current_dir(&project)
        .env("GOSSAMER_CACHE_DIR", &frontend)
        .env("GOSSAMER_CACHE", root.join("bindings"))
        .env("HOME", root.join("home"))
        .output()
        .expect("spawn cache status");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    assert!(stdout.contains("frontend"), "stdout: {stdout}");
    assert!(stdout.contains("1.5K"), "stdout: {stdout}");
    assert!(!stdout.contains("1536 bytes"), "stdout: {stdout}");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn cache_clear_removes_every_known_cache_class() {
    let root = env::temp_dir().join(format!(
        "gossamer-cache-clear-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let frontend = root.join("frontend");
    let shared_ir = root.join("ir-cache");
    let binding = root.join("binding-cache");
    let runners = binding.join("gossamer").join("runners");
    let packages = root.join("packages");
    let home = root.join("home");
    let build = home.join(".gossamer").join("build");
    let project = root.join("project");
    let project_ir = project.join(".gos-cache").join("ir-cache");
    let target = project.join("target");
    let vendor = project.join("vendor");

    for path in [&frontend, &shared_ir, &runners, &packages, &build, &project_ir] {
        std::fs::create_dir_all(path).expect("create cache root");
        std::fs::write(path.join("entry"), b"cache").expect("write cache entry");
    }
    for path in [&target, &vendor] {
        std::fs::create_dir_all(path).expect("create project directory");
        std::fs::write(path.join("entry"), b"project data").expect("write project data");
    }

    let out = Command::new(gos_bin())
        .args(["cache", "--clear"])
        .current_dir(&project)
        .env("GOSSAMER_CACHE_DIR", &frontend)
        .env("GOSSAMER_CACHE", &binding)
        .env("GOS_CACHE_DIR", &packages)
        .env("HOME", &home)
        .env_remove("XDG_CACHE_HOME")
        .output()
        .expect("spawn cache --clear");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("cache clear: removed"),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    for path in [&frontend, &shared_ir, &runners, &packages, &build, &project_ir] {
        assert!(!path.exists(), "cache root remains: {}", path.display());
    }
    assert!(target.exists(), "cache clear removed target/");
    assert!(vendor.exists(), "cache clear removed vendor/");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn parse_subcommand_round_trips_hello_world() {
    let fixture = write_fixture("parse", "fn main() { println(\"hello\") }\n");
    let out = Command::new(gos_bin())
        .args(["parse"])
        .arg(&fixture)
        .output()
        .expect("spawn parse");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    assert!(stdout.contains("fn main"));
    assert!(stdout.contains("println"));
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn check_subcommand_succeeds_on_simple_program() {
    let fixture = write_fixture(
        "check",
        "fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() { let _ = add(1i64, 2i64) }\n",
    );
    let out = Command::new(gos_bin())
        .args(["check"])
        .arg(&fixture)
        .output()
        .expect("spawn check");
    assert!(
        out.status.success(),
        "check failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("check: ok"));
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn check_subcommand_reports_type_mismatch() {
    let fixture = write_fixture("checkfail", "fn main() { let x: bool = 42i32 }\n");
    let out = Command::new(gos_bin())
        .args(["check"])
        .arg(&fixture)
        .output()
        .expect("spawn check");
    assert!(!out.status.success(), "expected failure");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("type: type mismatch") || stderr.contains("check failed"),
        "stderr: {stderr}"
    );
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn run_subcommand_executes_via_vm() {
    let fixture = write_fixture("run", "fn main() { println(\"cli-vm-run\") }\n");
    let out = Command::new(gos_bin())
        .args(["run"])
        .arg(&fixture)
        .output()
        .expect("spawn run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("cli-vm-run"));
    let _ = std::fs::remove_file(&fixture);
}

const LAZY_ITERATOR_TIER_SOURCE: &str = r#"use std::iter

fn main() {
    let xs = 1..100
        |> iter::map(|x| {
            if x > 3 { panic("map was eager") }
            x
        })
        |> iter::take(3)
        |> iter::collect
    println!("{}", iter::eager_sum(xs))

    let skipped = iter::range(1, 8) |> iter::skip(3) |> iter::take(2) |> iter::sum
    println!("{skipped}")

    let chained = iter::chain(iter::range(1, 3), iter::range(5, 7)) |> iter::sum
    println!("{chained}")

    let folded = iter::range(1, 5) |> iter::fold(10i64, |acc: i64, x: i64| acc + x)
    println!("{folded}")

    let any_hit = iter::range(1, 100)
        |> iter::any(|x| {
            if x > 4 { panic("any was eager") }
            x == 3
        })
    println!("{any_hit}")

    let all_hit = iter::range(1, 100)
        |> iter::all(|x| {
            if x > 4 { panic("all was eager") }
            x < 3
        })
    println!("{all_hit}")

    let found = iter::range(1, 100)
        |> iter::find(|x| {
            if x > 5 { panic("find was eager") }
            x == 4
        })
        |> option::unwrap_or(-1)
    println!("{found}")

    let once_sum = iter::once(41) |> iter::sum
    println!("{once_sum}")

    let product = iter::range(2, 5) |> iter::product
    println!("{product}")

    let min_value = iter::range(4, 7) |> iter::min |> option::unwrap_or(-1)
    println!("{min_value}")

    let max_value = iter::range(4, 7) |> iter::max |> option::unwrap_or(-1)
    println!("{max_value}")

    let enumerated = iter::range(3, 6) |> iter::enumerate |> iter::collect
    println!("{}", iter::eager_count(enumerated))

    let zipped = iter::zip(iter::range(1, 4), iter::range(10, 20)) |> iter::collect
    println!("{}", iter::eager_count(zipped))

    let pair_count = iter::range(1, 4) |> iter::enumerate |> iter::count
    println!("{pair_count}")

    let borrowed = [1, 2, 3, 4]
    let borrowed_total = borrowed
        |> iter::map(|x| x * 2)
        |> iter::filter(|x| x > 4)
        |> iter::take(2)
        |> iter::sum
    println!("{borrowed_total}")

    let mut replaced: Vec<i64> = [1, 2, 3]
    let pending_replacement = replaced |> iter::map(|x| x)
    replaced[1] = 9
    println!("{}", pending_replacement |> iter::sum)

    let open_end = 10..
        |> iter::take(4)
        |> iter::collect
    println!("{}", iter::eager_count(open_end))
}
"#;

const LAZY_ITERATOR_TIER_OUTPUT: &str =
    "6\n9\n14\n20\ntrue\nfalse\n4\n41\n24\n4\n6\n3\n3\n3\n14\n13\n4\n";

const EAGER_ITERATOR_ALIAS_SOURCE: &str = r#"use std::iter

fn main() {
    let exclusive = iter::eager_range(1, 5)
    let inclusive = iter::eager_range_inclusive(5, 7)
    let mapped = iter::eager_map(|x| x * 2, exclusive)
    let filtered = iter::eager_filter(|x| x > 3, mapped)
    let taken = iter::eager_take(2, filtered)
    let skipped = iter::eager_skip(1, taken)
    let enumerated = iter::eager_enumerate(skipped)
    let chained = iter::eager_chain(inclusive, [8, 9])
    let zipped = iter::eager_zip(chained, [1, 2, 3, 4, 5])
    let folded = iter::eager_fold(10i64, |acc: i64, x: i64| acc + x, [1, 2, 3])
    let any = iter::eager_any(|x| x == 2, [1, 2, 3])
    let all = iter::eager_all(|x| x > 0, [1, 2, 3])
    let found = iter::eager_find(|x| x == 2, [1, 2, 3]) |> option::unwrap_or(-1)
    let counted = iter::eager_count(enumerated)
    let collected = iter::eager_collect([4, 5, 6])
    let summed = iter::eager_sum(collected)
    println!("{folded} {any} {all} {found} {counted} {} {summed}", iter::eager_count(zipped))
}
"#;

const EAGER_ITERATOR_ALIAS_OUTPUT: &str = "16 true true 2 1 5 15\n";

const EAGER_2026_COMPAT_SOURCE: &str = r#"use std::iter

fn main() {
    let range = iter::range(2, 6)
    let mapped = range |> iter::map(|x| x * 2)
    let filtered = mapped |> iter::filter(|x| x > 5)
    let taken = filtered |> iter::take(2)
    println!("{} {} {} {}", range[0], mapped[1], taken[0], iter::sum(taken))
}
"#;

const EAGER_2026_COMPAT_OUTPUT: &str = "2 6 6 14\n";

// Allocation telemetry prints through the runtime's Unix-only `libc::atexit`
// hook; Windows still exercises lazy pipelines in the cross-tier tests.
#[cfg(unix)]
const LAZY_ITERATOR_ALLOCATION_SOURCE: &str = r#"use std::iter

fn main() {
    let out = iter::range(0, 100)
        |> iter::map(|x| x + 1)
        |> iter::filter(|x| x % 2 == 0)
        |> iter::take(3)
        |> iter::collect
    println!("{}", iter::eager_sum(out))
}
"#;

const LAZY_ITERATOR_INVALIDATION_SOURCE: &str = r#"use std::iter

fn main() {
    let mut xs: Vec<i64> = [1, 2, 3]
    let pending = xs |> iter::map(|x| x)
    xs.push(4)
    println!("{}", pending |> iter::sum)
}
"#;

const LAZY_ITERATOR_PANIC_SOURCE: &str = r#"use std::iter

fn main() {
    let _ = iter::range(0, 8)
        |> iter::map(|x| {
            if x == 3 { panic("lazy adapter panic sentinel") }
            x
        })
        |> iter::count
}
"#;

#[test]
fn run_absolute_project_uses_entry_edition_for_lazy_iterators() {
    let dir = env::temp_dir().join(format!("gos-lazy-edition-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create project dir");
    std::fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/lazy-edition\"\nversion = \"0.1.0\"\nedition = \"2027\"\n",
    )
    .expect("write manifest");
    std::fs::write(dir.join("main.gos"), LAZY_ITERATOR_TIER_SOURCE).expect("write source");
    let out = Command::new(gos_bin())
        .args(["run"])
        .arg(&dir)
        .output()
        .expect("spawn run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), LAZY_ITERATOR_TIER_OUTPUT);
    let jit_out = Command::new(gos_bin())
        .args(["run"])
        .arg(&dir)
        .env("GOSSAMER_JIT_THRESHOLD", "1")
        .output()
        .expect("spawn forced-jit run");
    assert!(
        jit_out.status.success(),
        "{}",
        String::from_utf8_lossy(&jit_out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&jit_out.stdout),
        LAZY_ITERATOR_TIER_OUTPUT
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_absolute_project_uses_entry_edition_for_lazy_iterators() {
    let dir = env::temp_dir().join(format!("gos-lazy-build-edition-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create project dir");
    std::fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/native-lazy-edition\"\nversion = \"0.1.0\"\nedition = \"2027\"\n",
    )
    .expect("write manifest");
    std::fs::write(dir.join("main.gos"), LAZY_ITERATOR_TIER_SOURCE).expect("write source");
    let build = Command::new(gos_bin())
        .args(["build"])
        .arg(&dir)
        .env("GOSSAMER_FAIL_ON_LLVM_FALLBACK", "1")
        .output()
        .expect("spawn build");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bin = dir.join("target").join("debug").join(if cfg!(windows) {
        "native-lazy-edition.exe"
    } else {
        "native-lazy-edition"
    });
    let out = Command::new(&bin).output().expect("run built binary");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), LAZY_ITERATOR_TIER_OUTPUT);

    let release_build = Command::new(gos_bin())
        .args(["build", "--release"])
        .arg(&dir)
        .env("GOSSAMER_FAIL_ON_LLVM_FALLBACK", "1")
        .output()
        .expect("spawn release build");
    assert!(
        release_build.status.success(),
        "{}",
        String::from_utf8_lossy(&release_build.stderr)
    );
    let release_bin = dir.join("target").join("release").join(if cfg!(windows) {
        "native-lazy-edition.exe"
    } else {
        "native-lazy-edition"
    });
    let release_out = Command::new(release_bin)
        .output()
        .expect("run release binary");
    assert!(
        release_out.status.success(),
        "{}",
        String::from_utf8_lossy(&release_out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&release_out.stdout),
        LAZY_ITERATOR_TIER_OUTPUT
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn eager_iterator_migration_aliases_run_on_vm_jit_and_llvm() {
    let dir = env::temp_dir().join(format!("gos-eager-iter-aliases-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create project dir");
    std::fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/eager-iter-aliases\"\nversion = \"0.1.0\"\nedition = \"2027\"\n",
    )
    .expect("write manifest");
    std::fs::write(dir.join("main.gos"), EAGER_ITERATOR_ALIAS_SOURCE).expect("write source");

    let vm = Command::new(gos_bin())
        .args(["run"])
        .arg(&dir)
        .output()
        .expect("spawn VM run");
    assert!(vm.status.success(), "{}", String::from_utf8_lossy(&vm.stderr));
    assert_eq!(String::from_utf8_lossy(&vm.stdout), EAGER_ITERATOR_ALIAS_OUTPUT);

    let jit = Command::new(gos_bin())
        .args(["run"])
        .arg(&dir)
        .env("GOSSAMER_JIT_THRESHOLD", "1")
        .output()
        .expect("spawn forced-JIT run");
    assert!(
        jit.status.success(),
        "{}",
        String::from_utf8_lossy(&jit.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&jit.stdout),
        EAGER_ITERATOR_ALIAS_OUTPUT
    );

    let build = Command::new(gos_bin())
        .args(["build"])
        .arg(&dir)
        .env("GOSSAMER_FAIL_ON_LLVM_FALLBACK", "1")
        .output()
        .expect("build LLVM fixture");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bin = dir.join("target").join("debug").join(if cfg!(windows) {
        "eager-iter-aliases.exe"
    } else {
        "eager-iter-aliases"
    });
    let llvm = Command::new(bin).output().expect("run LLVM fixture");
    assert!(
        llvm.status.success(),
        "{}",
        String::from_utf8_lossy(&llvm.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&llvm.stdout),
        EAGER_ITERATOR_ALIAS_OUTPUT
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn edition_2026_iterator_surface_remains_eager_on_all_tiers() {
    let dir = env::temp_dir().join(format!("gos-eager-iter-2026-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create project dir");
    std::fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/eager-iter-2026\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .expect("write manifest");
    std::fs::write(dir.join("main.gos"), EAGER_2026_COMPAT_SOURCE).expect("write source");

    for mut command in [
        {
            let mut command = Command::new(gos_bin());
            command.args(["run"]).arg(&dir);
            command
        },
        {
            let mut command = Command::new(gos_bin());
            command
                .args(["run"])
                .arg(&dir)
                .env("GOSSAMER_JIT_THRESHOLD", "1");
            command
        },
    ] {
        let out = command.output().expect("run eager compatibility fixture");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout), EAGER_2026_COMPAT_OUTPUT);
    }

    let build = Command::new(gos_bin())
        .args(["build"])
        .arg(&dir)
        .env("GOSSAMER_FAIL_ON_LLVM_FALLBACK", "1")
        .output()
        .expect("build eager compatibility fixture");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bin = dir.join("target").join("debug").join(if cfg!(windows) {
        "eager-iter-2026.exe"
    } else {
        "eager-iter-2026"
    });
    let llvm = Command::new(bin).output().expect("run LLVM fixture");
    assert!(
        llvm.status.success(),
        "{}",
        String::from_utf8_lossy(&llvm.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&llvm.stdout),
        EAGER_2026_COMPAT_OUTPUT
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
// See `LAZY_ITERATOR_ALLOCATION_SOURCE`: the assertion reads Unix-only exit
// telemetry, while functional lazy-pipeline coverage remains cross-platform.
#[cfg(unix)]
fn lazy_pipeline_allocates_only_its_collected_vec_on_llvm() {
    let dir = env::temp_dir().join(format!("gos-lazy-iter-allocs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create project dir");
    std::fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/lazy-iter-allocs\"\nversion = \"0.1.0\"\nedition = \"2027\"\n",
    )
    .expect("write manifest");
    std::fs::write(dir.join("main.gos"), LAZY_ITERATOR_ALLOCATION_SOURCE)
        .expect("write source");

    let build = Command::new(gos_bin())
        .args(["build"])
        .arg(&dir)
        .env("GOSSAMER_FAIL_ON_LLVM_FALLBACK", "1")
        .output()
        .expect("build allocation fixture");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bin = dir.join("target").join("debug").join(if cfg!(windows) {
        "lazy-iter-allocs.exe"
    } else {
        "lazy-iter-allocs"
    });
    let out = Command::new(bin)
        .env("GOS_VEC_ALLOC_STATS", "1")
        .output()
        .expect("run allocation fixture");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "12\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("VEC ALLOC STATS: inline=2 split=0 owner=0 region=0"),
        "expected one process bootstrap Vec plus the final collected Vec, got:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn borrowed_lazy_vec_structural_mutation_fails_on_all_tiers() {
    let dir = env::temp_dir().join(format!("gos-lazy-iter-invalidation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create project dir");
    std::fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/lazy-iter-invalidation\"\nversion = \"0.1.0\"\nedition = \"2027\"\n",
    )
    .expect("write manifest");
    std::fs::write(dir.join("main.gos"), LAZY_ITERATOR_INVALIDATION_SOURCE)
        .expect("write source");

    for mut command in [
        {
            let mut command = Command::new(gos_bin());
            command.args(["run"]).arg(&dir);
            command
        },
        {
            let mut command = Command::new(gos_bin());
            command
                .args(["run"])
                .arg(&dir)
                .env("GOSSAMER_JIT_THRESHOLD", "1");
            command
        },
    ] {
        let out = command.output().expect("run invalidation fixture");
        assert!(!out.status.success(), "unexpected stdout: {}", String::from_utf8_lossy(&out.stdout));
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains("borrowed Vec source was structurally mutated during iteration"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let build = Command::new(gos_bin())
        .args(["build"])
        .arg(&dir)
        .env("GOSSAMER_FAIL_ON_LLVM_FALLBACK", "1")
        .output()
        .expect("build invalidation fixture");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bin = dir.join("target").join("debug").join(if cfg!(windows) {
        "lazy-iter-invalidation.exe"
    } else {
        "lazy-iter-invalidation"
    });
    let llvm = Command::new(bin).output().expect("run LLVM fixture");
    assert!(!llvm.status.success());
    assert!(
        String::from_utf8_lossy(&llvm.stderr)
            .contains("borrowed Vec source was structurally mutated during iteration"),
        "{}",
        String::from_utf8_lossy(&llvm.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lazy_adapter_panic_propagates_on_all_tiers() {
    let dir = env::temp_dir().join(format!("gos-lazy-iter-panic-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create project dir");
    std::fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/lazy-iter-panic\"\nversion = \"0.1.0\"\nedition = \"2027\"\n",
    )
    .expect("write manifest");
    std::fs::write(dir.join("main.gos"), LAZY_ITERATOR_PANIC_SOURCE).expect("write source");

    for mut command in [
        {
            let mut command = Command::new(gos_bin());
            command.args(["run", "--no-jit"]).arg(&dir);
            command
        },
        {
            let mut command = Command::new(gos_bin());
            command
                .args(["run"])
                .arg(&dir)
                .env("GOSSAMER_JIT_THRESHOLD", "1")
                .env("GOSSAMER_JIT_MIN_WORK", "1");
            command
        },
    ] {
        let out = command.output().expect("run adapter panic fixture");
        assert!(!out.status.success(), "unexpected stdout: {}", String::from_utf8_lossy(&out.stdout));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("lazy adapter panic sentinel"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let build = Command::new(gos_bin())
        .args(["build"])
        .arg(&dir)
        .env("GOSSAMER_FAIL_ON_LLVM_FALLBACK", "1")
        .output()
        .expect("build adapter panic fixture");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bin = dir.join("target").join("debug").join(if cfg!(windows) {
        "lazy-iter-panic.exe"
    } else {
        "lazy-iter-panic"
    });
    let llvm = Command::new(bin).output().expect("run LLVM fixture");
    assert!(!llvm.status.success());
    assert!(
        String::from_utf8_lossy(&llvm.stderr).contains("lazy adapter panic sentinel"),
        "{}",
        String::from_utf8_lossy(&llvm.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stdin_read_line_appends_to_mut_string() {
    let fixture = write_fixture(
        "stdin-read-line",
        r#"use std::io

fn main() {
    let mut input = String::new()
    io::stdin().read_line(&mut input).unwrap()
    println!("typed={} bytes={}", input.trim(), input.len())
}
"#,
    );
    let mut child = Command::new(gos_bin())
        .args(["run"])
        .arg(&fixture)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn run");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b"hello\n")
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "typed=hello bytes=6\n"
    );
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn run_subcommand_executes_via_vm_by_default() {
    let fixture = write_fixture("runvm", "fn main() { println(\"cli-vm\") }\n");
    let out = Command::new(gos_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("spawn run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("cli-vm"));
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn build_subcommand_produces_runnable_output() {
    // `gos build` now defaults to native codegen via Cranelift + the
    // host `cc`. The happy-path output is a real executable that
    // exits with the Gossamer `main`'s return code. If native
    // codegen falls back (e.g. unsupported MIR), a launcher-script
    // takes over - both shapes are accepted here.
    let dir = env::temp_dir().join(format!("gos-build-magic-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("build_magic.gos");
    std::fs::write(&source_path, "fn main() -> i64 { 42i64 }\n").unwrap();
    let out = Command::new(gos_bin())
        .arg("build")
        .arg(&source_path)
        .output()
        .expect("spawn build");
    assert!(
        out.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let binary = dir
        .join("target")
        .join("debug")
        .join(format!("build_magic{}", std::env::consts::EXE_SUFFIX));
    assert!(
        binary.exists(),
        "build output missing at {}",
        binary.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&binary).unwrap().permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "output should be chmod +x: mode {mode:o}"
        );
    }
    // Either path prints a single build: line to stdout.
    assert!(String::from_utf8_lossy(&out.stdout).contains("build:"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(target_os = "linux")]
#[test]
fn build_rss_profile_reports_frontend_release_and_backend_peak() {
    let dir = env::temp_dir().join(format!("gos-build-rss-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("rss.gos");
    std::fs::write(&source_path, "fn main() { println(\"rss\") }\n").unwrap();

    let out = Command::new(gos_bin())
        .arg("build")
        .arg(&source_path)
        .env("GOS_PROFILE_RSS", "1")
        .output()
        .expect("spawn build with RSS profiling");
    assert!(
        out.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    for stage in [
        "build_frontend_checked",
        "build_frontend_released",
        "build_backend_emitted",
    ] {
        assert!(stderr.contains(&format!("rss: stage={stage} ")), "{stderr}");
    }
    assert!(stderr.contains("peak_bytes="), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_build_records_15_0_deployment_target() {
    let dir = env::temp_dir().join(format!(
        "gos-macos-deployment-target-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("deployment_target.gos");
    std::fs::write(&source_path, "fn main() { println(\"macos-15\") }\n").unwrap();

    let build = Command::new(gos_bin())
        .arg("build")
        .arg(&source_path)
        .env_remove("MACOSX_DEPLOYMENT_TARGET")
        .output()
        .expect("spawn gos build");
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let binary = dir
        .join("target")
        .join("debug")
        .join("deployment_target");
    let metadata = Command::new("otool")
        .arg("-l")
        .arg(&binary)
        .output()
        .expect("run otool");
    assert!(
        metadata.status.success(),
        "otool failed: {}",
        String::from_utf8_lossy(&metadata.stderr)
    );
    let metadata = String::from_utf8(metadata.stdout).expect("otool output is UTF-8");
    assert!(
        metadata.lines().any(|line| line.trim() == "minos 15.0"),
        "Mach-O does not record macOS 15.0 as LC_BUILD_VERSION minos:\n{metadata}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_output_handles_empty_argv_for_flag_define_programs() {
    let dir = env::temp_dir().join(format!("gos-build-argv-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("argv_ok.gos");
    std::fs::write(
        &source_path,
        "use std::flag\n\
         fn main() {\n\
             let flags = flag::define(\"argv-ok\", [\n\
                 flag::int(\"port\", 8080, \"port\", 'p'),\n\
                 flag::bool(\"verbose\", false, \"verbose\", 'v'),\n\
             ])\n\
             if *flags.verbose {\n\
                 println(\"verbose\")\n\
             } else {\n\
                 println((*flags.port).to_string())\n\
             }\n\
         }\n",
    )
    .unwrap();
    let build = Command::new(gos_bin())
        .arg("build")
        .arg(&source_path)
        .output()
        .expect("spawn build");
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let binary = dir
        .join("target")
        .join("debug")
        .join(format!("argv_ok{}", std::env::consts::EXE_SUFFIX));
    let run = Command::new(&binary).output().expect("run built artifact");
    assert!(
        run.status.success(),
        "native run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "8080\n");
}

#[test]
fn build_output_preserves_http_method_chain_through_send_and_field_access() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback server");
    let addr = listener.local_addr().expect("loopback addr");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept client");
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello")
            .expect("write response");
    });

    let dir = env::temp_dir().join(format!("gos-build-http-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("http_chain.gos");
    std::fs::write(
        &source_path,
        format!(
            "use std::http\n\
             fn main() {{\n\
                 let url = \"http://{addr}/\".to_string()\n\
                 match http::Client::new().get(&url).send() {{\n\
                     Ok(resp) => println(resp.status.to_string() + \":\" + resp.body),\n\
                     Err(e) => println(\"send failed: \" + e.message()),\n\
                 }}\n\
             }}\n"
        ),
    )
    .unwrap();
    let build = Command::new(gos_bin())
        .arg("build")
        .arg(&source_path)
        .output()
        .expect("spawn build");
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let binary = dir
        .join("target")
        .join("debug")
        .join(format!("http_chain{}", std::env::consts::EXE_SUFFIX));
    let run = Command::new(&binary).output().expect("run built artifact");
    assert!(
        run.status.success(),
        "native run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "200:hello\n");
    server.join().expect("join server");
}

/// Serves `count` HTTP requests on `listener`, echoing the `x-test`
/// header and the request body back as `xt=<v> body=<b>` with a 201.
fn serve_builder_echo(listener: &TcpListener, count: usize) {
    for _ in 0..count {
        let (mut stream, _) = listener.accept().expect("accept client");
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        let body_start = loop {
            let n = stream.read(&mut chunk).expect("read request");
            buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
            assert!(n != 0, "connection closed before headers completed");
        };
        let lower = String::from_utf8_lossy(&buf[..body_start]).to_ascii_lowercase();
        let content_len: usize = lower
            .lines()
            .find_map(|l| l.strip_prefix("content-length:").map(str::trim))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        while buf.len() < body_start + content_len {
            let n = stream.read(&mut chunk).expect("read body");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let xt = lower
            .lines()
            .find_map(|l| l.strip_prefix("x-test:").map(str::trim))
            .unwrap_or("<none>");
        let body = String::from_utf8_lossy(&buf[body_start..]).into_owned();
        let reply = format!("xt={xt} body={body}");
        let resp = format!(
            "HTTP/1.1 201 Created\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            reply.len(),
            reply
        );
        stream.write_all(resp.as_bytes()).expect("write response");
    }
}

/// Tier-parity sentinel for the chained client builder: the same
/// source must produce byte-identical stdout under `gos run` (VM)
/// and a `gos build` native binary, with the chained header + body
/// honored and a transport failure surfacing as `Err` on both tiers.
#[test]
fn vm_and_native_client_builder_chain_outputs_match() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback server");
    let addr = listener.local_addr().expect("loopback addr");
    // One POST per tier (VM run + native run).
    let server = std::thread::spawn(move || serve_builder_echo(&listener, 2));

    let source = format!(
        "use std::http\n\
         fn main() {{\n\
             let client = http::Client::new()\n\
             let sent = client\n\
                 .post(&\"http://{addr}/echo\")\n\
                 .header(\"x-test\", \"parity\")\n\
                 .body(\"ping\")\n\
                 .send()\n\
             match sent {{\n\
                 Ok(r) => println!(\"post: {{}} {{}}\", r.status, r.body),\n\
                 Err(e) => println!(\"post err: {{}}\", e),\n\
             }}\n\
             match client.get(&\"http://127.0.0.1:1/refused\").send() {{\n\
                 Ok(r) => println!(\"refused ok: {{}}\", r.status),\n\
                 Err(e) => println!(\"refused err: {{}}\", e),\n\
             }}\n\
         }}\n"
    );
    let dir = env::temp_dir().join(format!("gos-builder-parity-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("builder_parity.gos");
    std::fs::write(&source_path, source).unwrap();

    let vm = Command::new(gos_bin())
        .arg("run")
        .arg(&source_path)
        .output()
        .expect("spawn gos run");
    assert!(
        vm.status.success(),
        "gos run failed: {}",
        String::from_utf8_lossy(&vm.stderr)
    );

    let build = Command::new(gos_bin())
        .arg("build")
        .arg(&source_path)
        .output()
        .expect("spawn build");
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let binary = dir
        .join("target")
        .join("debug")
        .join(format!("builder_parity{}", std::env::consts::EXE_SUFFIX));
    let native = Command::new(&binary).output().expect("run built artifact");
    assert!(
        native.status.success(),
        "native run failed: {}",
        String::from_utf8_lossy(&native.stderr)
    );

    let vm_out = String::from_utf8_lossy(&vm.stdout).into_owned();
    let native_out = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(vm_out, native_out, "tier outputs diverge");
    assert!(
        vm_out.contains("post: 201 xt=parity body=ping"),
        "chained header/body not honored: {vm_out}"
    );
    assert!(
        vm_out.contains("refused err: http: transport:"),
        "transport failure must surface as Err: {vm_out}"
    );
    server.join().expect("join server");
}

#[test]
fn build_subcommand_accepts_known_target_triple_and_rejects_unknown() {
    let dir = env::temp_dir().join(format!("gos-build-cross-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("cross.gos");
    std::fs::write(&source_path, "fn main() -> i64 { 0i64 }\n").unwrap();
    // A registered Linux cross target is routed into the real build
    // path. Without a target runtime archive (and a cross linker) it
    // fails at link resolution with a clear message - never the
    // registration-gate "unknown target" error, and never a stub.
    let known = Command::new(gos_bin())
        .args(["build", "--target", "aarch64-unknown-linux-gnu"])
        .arg(&source_path)
        .output()
        .expect("spawn build --target");
    let known_err = String::from_utf8_lossy(&known.stderr);
    assert!(
        !known_err.contains("unknown target"),
        "a registered Linux target must pass the registration gate: {known_err}"
    );
    // A registered but non-Linux target cannot be cross-produced from
    // any host (no bundled SDK); it is refused with a specific error,
    // not silently stubbed. Pick the darwin triple for the *other*
    // arch: on an Apple Silicon macOS runner (host `aarch64-apple-darwin`,
    // what `macos-latest` is today) the same-arch triple equals the host
    // and takes the native, non-cross build path instead of being refused.
    let other_arch_darwin = if cfg!(target_arch = "aarch64") {
        "x86_64-apple-darwin"
    } else {
        "aarch64-apple-darwin"
    };
    let darwin = Command::new(gos_bin())
        .args(["build", "--target", other_arch_darwin])
        .arg(&source_path)
        .output()
        .expect("spawn build --target darwin");
    assert!(
        !darwin.status.success(),
        "a non-Linux cross target must be refused"
    );
    let darwin_err = String::from_utf8_lossy(&darwin.stderr);
    assert!(
        darwin_err.contains("only `*-linux-*`"),
        "non-Linux target should be refused with a specific message: {darwin_err}"
    );
    let bad = Command::new(gos_bin())
        .args(["build", "--target", "wat-is-this"])
        .arg(&source_path)
        .output()
        .expect("spawn build --target bad");
    assert!(
        !bad.status.success(),
        "unknown target should fail the build"
    );
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("unknown target"),
        "stderr should name the unknown-target error"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_defaults_output_to_source_stem_without_extension() {
    // `gos build line_count.gos` should write a file called
    // `line_count` (the executable produced by the native codegen
    // pipeline).
    let dir = env::temp_dir().join(format!("gos-build-default-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("line_count.gos");
    std::fs::write(&source_path, "fn main() -> i64 { 0i64 }\n").unwrap();
    let out = Command::new(gos_bin())
        .arg("build")
        .arg(&source_path)
        .output()
        .expect("spawn build");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let binary = dir
        .join("target")
        .join("debug")
        .join(format!("line_count{}", std::env::consts::EXE_SUFFIX));
    assert!(
        binary.exists(),
        "expected build output at {}",
        binary.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_honours_project_output_field_in_manifest() {
    let dir = env::temp_dir().join(format!("gos-build-manifest-out-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/widget\"\nversion = \"0.1.0\"\noutput = \"custom_name\"\n",
    )
    .unwrap();
    let source_path = dir.join("src/main.gos");
    std::fs::write(&source_path, "fn main() -> i64 { 0i64 }\n").unwrap();
    let out = Command::new(gos_bin())
        .arg("build")
        .arg(&source_path)
        .output()
        .expect("spawn build");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The manifest `output` has no extension; on Windows the linker needs
    // the `.exe` suffix, which `resolve_output_path` adds. Expect the
    // platform executable name, not the bare stem.
    let expected_name = if cfg!(windows) {
        "custom_name.exe"
    } else {
        "custom_name"
    };
    let expected = dir.join(expected_name);
    assert!(
        expected.exists(),
        "expected build output at {}",
        expected.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_inside_project_names_binary_after_project_id_tail() {
    // Rust's convention: `cargo build` writes `target/debug/<package>`,
    // not `target/debug/main`. Gossamer follows the same rule when a
    // `project.toml` is present - the binary takes the last segment
    // of `[project] id`, regardless of which source file holds `main`.
    let dir = env::temp_dir().join(format!("gos-build-id-tail-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"github.com/acme/widget-cli\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let source_path = dir.join("src/main.gos");
    std::fs::write(&source_path, "fn main() { }\n").unwrap();
    let out = Command::new(gos_bin())
        .arg("build")
        .arg(&source_path)
        .output()
        .expect("spawn build");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let expected = dir
        .join("target")
        .join("debug")
        .join(format!("widget-cli{}", std::env::consts::EXE_SUFFIX));
    assert!(
        expected.exists(),
        "expected build output at {}",
        expected.display()
    );
    let stale = dir
        .join("target")
        .join("debug")
        .join(format!("main{}", std::env::consts::EXE_SUFFIX));
    assert!(
        !stale.exists(),
        "binary must not be named after the source file when a manifest exists"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_rejects_removed_output_flag() {
    let fixture = write_fixture("buildflagremoved", "fn main() { }\n");
    let out = Command::new(gos_bin())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg("somewhere")
        .output()
        .expect("spawn build");
    assert!(!out.status.success(), "-o should not be accepted");
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn update_is_a_first_class_package_command() {
    let out = Command::new(gos_bin())
        .args(["update", "--help"])
        .output()
        .expect("spawn update help");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("newest dependency versions"), "{stdout}");
    assert!(stdout.contains("--offline"), "{stdout}");
}

#[test]
fn tidy_removes_only_unimported_project_dependencies() {
    let dir = env::temp_dir().join(format!("gos-tidy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let manifest = dir.join("project.toml");
    std::fs::write(
        &manifest,
        "[project]\nid = \"example.com/app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"example.com/used\" = \"1.0.0\"\n\"example.com/unused\" = \"1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.gos"),
        "use \"example.com/used\" as used\nfn main() { used::run() }\n",
    )
    .unwrap();

    let out = Command::new(gos_bin())
        .args(["tidy", "--manifest"])
        .arg(&manifest)
        .output()
        .expect("spawn tidy");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rewritten = std::fs::read_to_string(&manifest).unwrap();
    assert!(rewritten.contains("example.com/used"), "{rewritten}");
    assert!(!rewritten.contains("example.com/unused"), "{rewritten}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 unused dependency/dependencies removed")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tidy_does_not_edit_manifest_when_a_source_file_has_parse_errors() {
    let dir = env::temp_dir().join(format!("gos-tidy-parse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let manifest = dir.join("project.toml");
    let original = "[project]\nid = \"example.com/app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"example.com/keep\" = \"1.0.0\"\n";
    std::fs::write(&manifest, original).unwrap();
    std::fs::write(dir.join("src/main.gos"), "fn main( {\n").unwrap();

    let out = Command::new(gos_bin())
        .args(["tidy", "--manifest"])
        .arg(&manifest)
        .output()
        .expect("spawn tidy");
    assert!(!out.status.success(), "tidy must reject malformed source");
    assert_eq!(std::fs::read_to_string(&manifest).unwrap(), original);
    let _ = std::fs::remove_dir_all(&dir);
}
