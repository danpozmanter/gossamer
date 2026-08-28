//! What a build costs for the code it does not use.
//!
//! Everything after lowering is paid per body, so a program that defines
//! two thousand functions and calls one used to pay for two thousand
//! through MIR optimisation, IR emission, and LLVM. The reachability
//! pass drops them, and these are the gates that keep it doing so.
//!
//! The cost case is stated as a ratio against a hello-world build
//! measured in the same run rather than as a number of seconds: a shared
//! CI runner's absolute speed is not a property of this compiler, but
//! the two builds' relation to each other is.

#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::process::Command;
fn gos_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_gos"))
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonicalize workspace root")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gos-build-time-{pid}-{tag}",
        pid = std::process::id(),
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// One cold `gos build --release` of `fixture`: how large the binary is,
/// and the timings line's JSON.
struct BuildMeasurement {
    binary_bytes: u64,
    timings: String,
}

fn build(fixture: &str, tag: &str) -> BuildMeasurement {
    build_with_pass(fixture, tag, true)
}

/// One cold build with the reachability pass on or off.
fn build_with_pass(fixture: &str, tag: &str, pass: bool) -> BuildMeasurement {
    let source = workspace_root().join("benchmarks/build_time").join(fixture);
    let dir = scratch(tag);
    let copied = dir.join(fixture);
    // The toolchain caches a build by the hash of its source, so a
    // second build of the same fixture would be served from that cache
    // and time nothing. One comment line naming this measurement makes
    // each one a program the cache has never seen.
    let text = std::fs::read_to_string(&source)
        .unwrap_or_else(|e| panic!("read {}: {e}", source.display()));
    std::fs::write(&copied, format!("// measurement: {tag}\n{text}"))
        .expect("write the fixture into a scratch directory");

    let mut command = Command::new(gos_bin());
    command
        .args(["build", "--release", "--timings"])
        .arg(&copied);
    if pass {
        command.env_remove("GOS_MIR_NO_DCE");
    } else {
        command.env("GOS_MIR_NO_DCE", "1");
    }
    let output = command.output().expect("run gos build");
    assert!(
        output.status.success(),
        "building {fixture} failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stem = Path::new(fixture)
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("fixture stem");
    let binary = dir
        .join("target")
        .join("release")
        .join(format!("{stem}{}", std::env::consts::EXE_SUFFIX));
    let binary_bytes = std::fs::metadata(&binary)
        .unwrap_or_else(|e| panic!("stat {}: {e}", binary.display()))
        .len();
    let timings = String::from_utf8_lossy(&output.stdout)
        .lines()
        .find(|line| line.starts_with("build-timings:"))
        .unwrap_or_default()
        .to_string();
    assert!(
        timings.contains("\"final_artifact_cache_hit\":false"),
        "{tag} was served from the artifact cache, so it timed nothing: {timings:?}",
    );
    let _ = std::fs::remove_dir_all(&dir);
    BuildMeasurement {
        binary_bytes,
        timings,
    }
}

/// The integer a `"key":N` pair carries in the timings line.
fn field(timings: &str, key: &str) -> u64 {
    let needle = format!("\"{key}\":");
    let rest = timings
        .split(&needle)
        .nth(1)
        .unwrap_or_else(|| panic!("{key} missing from {timings:?}"));
    rest.chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or_else(|_| panic!("{key} is not a number in {timings:?}"))
}

/// Two thousand unreached functions must not reach code generation. The
/// count is exact rather than approximate: every one of them is dead by
/// construction, and a pass that dropped some but not others would be
/// answering a question nobody asked.
#[test]
fn unused_functions_are_not_lowered_to_native_code() {
    let many = build("many_unused.gos", "many-count");
    let pruned = field(&many.timings, "pruned_count");
    assert!(
        pruned >= 2000,
        "only {pruned} of the 2000 unreached functions were pruned: {}",
        many.timings,
    );
}

/// A binary carrying two thousand functions nothing calls is no larger than
/// one that does not.
///
/// The control is the same program with the dead functions removed - same
/// entry, same reachable function, same formatting path - so the difference
/// between the two is the unreached code and nothing else. Measuring against
/// a program that prints something else would also measure the difference
/// between their two runtime paths, which is a real difference on some
/// targets and none of this gate's business.
#[test]
fn unused_functions_do_not_reach_the_binary() {
    let control = build("one_used.gos", "one-used-size");
    let many = build("many_unused.gos", "many-size");
    let allowed = control.binary_bytes + control.binary_bytes / 100;
    assert!(
        many.binary_bytes <= allowed,
        "the binary with 2000 unused functions is {} bytes against the same program \
         without them at {} (more than 1% larger), so unreached code is still being \
         emitted",
        many.binary_bytes,
        control.binary_bytes,
    );
}

/// The gate on what reaches the backend at all: the same fixture built
/// twice, once with the reachability pass and once with
/// `GOS_MIR_NO_DCE=1`.
///
/// Stated as a body count rather than as a time. Everything downstream
/// of this number - MIR optimisation, RC insertion, IR text, and LLVM's
/// own work - is paid per body, and the count is exact on every host and
/// in every profile, which a duration is not: the toolchain caches
/// per-body objects, so a second build of the same bodies restores them
/// rather than compiling them, and the measured saving would be whatever
/// the cache happened to hold. The durations are recorded in
/// `~/dev/plans/gos_0570_todo.md`, measured on a cold tree.
#[test]
fn unreached_code_never_reaches_the_backend() {
    let off = build_with_pass("many_unused.gos", "many-bodies-off", false);
    let on = build_with_pass("many_unused.gos", "many-bodies-on", true);
    assert_eq!(field(&off.timings, "pruned_count"), 0, "{}", off.timings);
    assert_eq!(
        field(&off.timings, "body_count"),
        2002,
        "with the pass off, every function should reach the backend: {}",
        off.timings,
    );
    assert_eq!(
        field(&on.timings, "body_count"),
        2,
        "only `main` and the one function it calls should reach the backend: {}",
        on.timings,
    );
}
