#!/usr/bin/env bash
# Pre-commit gate. Mirrors the CI workflows in `.github/workflows/`
# so failures surface locally before they hit a runner:
#
#   ci.yml          → fmt, clippy, test, doctests, rustdoc (broken
#                     intra-doc-links), cross-target check (wasm32 +
#                     musl-via-zigbuild), audit, deny
#   sanitizers.yml  → ASan + TSan on the unsafe-touching crates
#   fuzz.yml        → 10 s smoke per target
#
# By default each step's chatter is suppressed; pass `--full` to see
# every line. Any step's non-zero exit replays the captured output
# before bailing so the failure is debuggable.
#
# Flags to skip slow gates on dev machines:
#   --no-sanitizers   skip ASan / TSan (need nightly + rust-src)
#   --no-fuzz         skip the fuzz smoke
#   --no-cross        skip the wasm32 cross-target check + musl zigbuild check
#   --no-audit        skip cargo-audit (needs cargo-audit installed)
#   --no-deny         skip cargo-deny  (needs cargo-deny installed)
#   --no-doctests     skip `cargo test --doc --workspace --release`
#   --no-rustdoc      skip `cargo doc -D rustdoc::broken_intra_doc_links`
#
# Missing optional tools cause a clean skip rather than a failure;
# everything that *can* run, runs.
set -euo pipefail

# Force full backtraces so a panicking step (e.g. a failed `cargo test`)
# replays its stack in the captured output, not just the panic message and
# a note to set this. A caller's explicit RUST_BACKTRACE wins.
export RUST_BACKTRACE="${RUST_BACKTRACE:-full}"

full=0
run_sanitizers=1
run_fuzz=1
run_cross=1
run_audit=1
run_deny=1
run_doctests=1
run_rustdoc=1
for arg in "$@"; do
    case "$arg" in
        --full)          full=1 ;;
        --no-sanitizers) run_sanitizers=0 ;;
        --no-fuzz)       run_fuzz=0 ;;
        --no-cross)      run_cross=0 ;;
        --no-audit)      run_audit=0 ;;
        --no-deny)       run_deny=0 ;;
        --no-doctests)   run_doctests=0 ;;
        --no-rustdoc)    run_rustdoc=0 ;;
        -h|--help)
            sed -n '/^# Pre-commit gate/,/^set -euo pipefail/p' "$0" | sed 's/^# \{0,1\}//' | head -n -1
            exit 0
            ;;
        *)
            echo "unknown arg: $arg (try --help)" >&2
            exit 2
            ;;
    esac
done

# Run a step and surface only warnings / errors by default. Each
# step's stdout+stderr is captured to a temp file; if the step
# exits non-zero we print the whole capture so the failure is
# diagnosable. On success we grep the capture for warning / error
# lines and emit those (cargo and clang-style diagnostics both use
# `error:` / `warning:` prefixes, which is what we match).
run_step() {
    local label="$1"
    shift
    local log
    log="$(mktemp)"
    echo "==> $label"
    if [[ $full -eq 1 ]]; then
        if ! "$@" 2>&1 | tee "$log"; then
            rm -f "$log"
            exit 1
        fi
    else
        if ! "$@" >"$log" 2>&1; then
            echo "$label FAILED - full output:" >&2
            cat "$log" >&2
            rm -f "$log"
            exit 1
        fi
        # Surface warning / error lines (plus a couple of lines of
        # following context - diagnostics usually print the source
        # excerpt right after the header) so problems aren't silent
        # even when the step succeeds with warnings.
        grep -E -i -A 2 '^(warning|error)[:\[]|: warning:|: error:' "$log" || true
    fi
    rm -f "$log"
}

run_step "cargo fmt"                                       cargo fmt
run_step "cargo clippy --workspace --all-targets"          cargo clippy --workspace --all-targets -- -D warnings
run_step "cargo test --workspace --no-fail-fast"           cargo test --workspace --no-fail-fast
# Stdlib docs drift gate - verifies docs_src/stdlib/ pages match
# what `manifest::ALL_MODULES` would emit. Build the binary first
# so the check uses the freshly built crate.
run_step "cargo build --bin gos"                           cargo build --bin gos
run_step "gos doc --emit-stdlib --check"                   ./target/debug/gos doc --emit-stdlib docs_src/stdlib --check
# Feature-status sanity - every `Experimental` registry entry has a
# doc page on disk. (Shipped items also need a passing tier-parity
# sidecar; that requires the full cross-tier walk and is gated by
# the dedicated `gos test --tier-parity --report=status` job rather
# than this fast pre-commit pass.)
run_step "gos feature-status --status experimental --check" ./target/debug/gos feature-status --status experimental --check

# Rustdoc broken-intra-doc-links gate - mirrors the docs job in
# `.github/workflows/ci.yml`. Wired here so internal-doc drift
# (links to renamed or now-private items) fails locally instead of
# surfacing in CI as a red post-push status.
if [[ $run_rustdoc -eq 1 ]]; then
    # `--document-private-items` matches the CI `cargo-doc` job exactly:
    # without it rustdoc skips private items entirely, so a broken
    # intra-doc link in a private fn's doc comment (as opposed to a pub
    # one) would pass locally and only fail once it reached CI.
    RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" \
        run_step "cargo doc --workspace --no-deps --document-private-items" \
        cargo doc --workspace --no-deps --document-private-items
fi

# Doctest gate - mirrors `cargo test --doc --workspace --release`
# in ci.yml. Catches stale `///` examples that don't compile.
if [[ $run_doctests -eq 1 ]]; then
    run_step "cargo test --doc --workspace --release" \
        cargo test --doc --workspace --release
fi

# cargo-deny - license + advisory + bans + sources gate
# (`.github/workflows/ci.yml` deny job). Skip cleanly if
# `cargo-deny` isn't installed so the local pass keeps moving.
if [[ $run_deny -eq 1 ]]; then
    if command -v cargo-deny >/dev/null 2>&1; then
        run_step "cargo deny check" cargo deny check
    else
        echo "cargo deny skipped (run \`cargo install cargo-deny\` to enable)"
    fi
fi

# cargo-audit - RUSTSEC advisory gate (`.github/workflows/ci.yml`
# audit job). Skip cleanly if `cargo-audit` isn't installed.
if [[ $run_audit -eq 1 ]]; then
    if command -v cargo-audit >/dev/null 2>&1; then
        run_step "cargo audit" cargo audit
    else
        echo "cargo audit skipped (run \`cargo install cargo-audit --locked\` to enable)"
    fi
fi

# Cross-target check - mirrors the cross-targets job's wasm32 leg.
# Just the wasm-portable crates: rustls / corosensei / mio aren't
# wasm-clean, so runtime / sched / binding / pkg can't be asked to
# compile there. The Linux cross targets (aarch64-gnu, riscv64-gnu)
# need a target-prefixed gcc that's hard to expect on dev machines
# - those stay CI-only.
if [[ $run_cross -eq 1 ]]; then
    if rustup target list --installed 2>/dev/null | grep -q '^wasm32-unknown-unknown$'; then
        run_step "cargo check --target wasm32-unknown-unknown (wasm-portable crates)" \
            cargo check -p gossamer-abi -p gossamer-binding-macros \
            -p gossamer-interp -p gossamer-playground \
            --target wasm32-unknown-unknown
    else
        echo "cross-target check skipped (run \`rustup target add wasm32-unknown-unknown\` to enable)"
    fi

    # Musl cross-compile check (via `cargo zigbuild`) - mirrors the "Build
    # the target runtime archives" step shared by cross-from-linux/-macos/
    # -windows in ci.yml. Exercises the native C deps (ring, mimalloc,
    # zstd) that only fail to cross-compile at actual build time, not at
    # `cargo check` time: a bare cross C compiler has no musl sysroot, and
    # even `CC=zig cc` doesn't work standalone (cc-rs's own appended
    # `--target=<rustc-triple>` flag is a form zig's parser rejects, and it
    # wins over whatever `-target` we pass through `CC`) - `cargo zigbuild`
    # is the maintained tool that reconciles the two. Skips cleanly when
    # zig (>=0.9.0, cargo-zigbuild's own floor - a distro-packaged `zig`
    # binary is commonly older), `cargo-zigbuild`, or the musl rustup
    # targets aren't installed, so an unrelated stale system `zig` never
    # turns into a hard failure here.
    zig_usable=0
    if command -v zig >/dev/null 2>&1 && command -v cargo-zigbuild >/dev/null 2>&1; then
        zig_minor="$(zig version 2>/dev/null | awk -F'[.-]' '{print ($1 > 0) ? 9 : $2}')"
        [[ "${zig_minor:-0}" =~ ^[0-9]+$ && "$zig_minor" -ge 9 ]] && zig_usable=1
    fi
    if [[ $zig_usable -eq 1 ]]; then
        for t in aarch64-unknown-linux-musl x86_64-unknown-linux-musl; do
            if rustup target list --installed 2>/dev/null | grep -q "^${t}$"; then
                run_step "cargo zigbuild --release --target $t -p gossamer-runtime" \
                    cargo zigbuild --release --target "$t" -p gossamer-runtime
            else
                echo "musl cross check for $t skipped (run \`rustup target add $t\` to enable)"
            fi
        done
    else
        echo "musl cross check skipped (install zig >=0.9.0 + \`cargo install cargo-zigbuild\` to enable)"
    fi
fi

# ASan / TSan - mirrors `.github/workflows/sanitizers.yml`. Needs a
# nightly toolchain with the `rust-src` component (so `-Z build-std`
# can recompile std + the sanitizer runtimes). CI pins a specific
# nightly date for reproducibility; locally we honor that pin when
# it's installed and otherwise fall back to plain `nightly` so dev
# machines stay usable without an extra rustup install.
#
# Discovery order:
#   1. Pinned `nightly-2026-04-14` toolchain (matches CI exactly).
#   2. Plain `nightly` (anything `rustup toolchain list` calls
#      "nightly-..." that isn't the date pin) - runs the same gates,
#      just under whatever nightly is on the dev box.
#   3. Skip with a one-line install hint.
if [[ $run_sanitizers -eq 1 ]]; then
    asan_pinned="nightly-2026-04-14"
    asan_toolchain=""
    if rustup toolchain list 2>/dev/null | grep -q "^${asan_pinned}"; then
        asan_toolchain="$asan_pinned"
    else
        # Pick the first `nightly-*` line. `nightly` (no triple) and
        # `nightly-<triple>` both qualify; rustup canonicalises to
        # the triple form on most installs.
        asan_toolchain="$(rustup toolchain list 2>/dev/null \
            | awk '/^nightly/{ sub(/ .*/, ""); print; exit }')"
        if [[ -n "$asan_toolchain" && "$asan_toolchain" != "$asan_pinned" ]]; then
            echo "sanitizers: pinned $asan_pinned not installed; falling back to $asan_toolchain"
            echo "            CI uses the pinned date - install with"
            echo "              rustup toolchain install $asan_pinned --component rust-src"
        fi
    fi
    if [[ -z "$asan_toolchain" ]]; then
        echo "sanitizers skipped (no nightly toolchain - install with"
        echo "  rustup toolchain install $asan_pinned --component rust-src)"
    elif ! rustup component list --installed --toolchain "$asan_toolchain" 2>/dev/null \
            | grep -q rust-src; then
        echo "sanitizers skipped (run \`rustup component add rust-src --toolchain $asan_toolchain\` to enable)"
    else
        # ASan tests touch sigaltstack-dependent code in
        # gossamer-runtime; the public `stack_guard::install()`
        # honors ASAN_OPTIONS by skipping our handler so libasan's
        # signal stack stays the only one in play.
        RUSTFLAGS="-Z sanitizer=address" \
        RUSTDOCFLAGS="-Z sanitizer=address" \
        ASAN_OPTIONS="detect_leaks=0:abort_on_error=1:halt_on_error=1" \
            run_step "cargo +$asan_toolchain test (ASan)" \
            cargo "+$asan_toolchain" test \
                -Z build-std \
                --target x86_64-unknown-linux-gnu \
                --lib \
                -p gossamer-runtime \
                -p gossamer-interp \
                -p gossamer-coro \
                -p gossamer-mir \
                -p gossamer-binding

        RUSTFLAGS="-Z sanitizer=thread" \
        RUSTDOCFLAGS="-Z sanitizer=thread" \
        TSAN_OPTIONS="halt_on_error=1:second_deadlock_stack=1" \
            run_step "cargo +$asan_toolchain test (TSan)" \
            cargo "+$asan_toolchain" test \
                -Z build-std \
                --target x86_64-unknown-linux-gnu \
                --lib \
                -p gossamer-runtime \
                -p gossamer-sched \
                -p gossamer-coro
    fi
fi

# Fuzz smoke - mirrors `.github/workflows/fuzz.yml` so adversarial
# inputs that CI would flag also fail locally. Each target runs
# briefly (10 s by default; override with GOSSAMER_FUZZ_SECS) and
# replays its seed corpus. Skip cleanly when cargo-fuzz or the
# nightly toolchain isn't installed so the rest of `check.sh`
# stays useful on dev machines that haven't set the harness up.
fuzz_secs="${GOSSAMER_FUZZ_SECS:-10}"
if [[ $run_fuzz -eq 1 ]] && command -v cargo-fuzz >/dev/null 2>&1 && rustup toolchain list 2>/dev/null | grep -q '^nightly'; then
    echo "==> fuzz smoke (${fuzz_secs}s per target)"
    fuzz_log="$(mktemp -d)/fuzz.log"
    for target in lex parse manifest http_request typecheck resolve mir_lower hir_lower vm_compile vm_run; do
        if [[ $full -eq 1 ]]; then
            echo "  -> $target"
            if ! ( cd fuzz && cargo +nightly fuzz run "$target" -- \
                    -max_total_time="$fuzz_secs" -max_len=65536 -rss_limit_mb=2048 -malloc_limit_mb=2048 -timeout=30 ); then
                exit 1
            fi
        else
            if ! ( cd fuzz && cargo +nightly fuzz run "$target" -- \
                    -max_total_time="$fuzz_secs" -max_len=65536 -rss_limit_mb=2048 -malloc_limit_mb=2048 -timeout=30 ) \
                    >"$fuzz_log" 2>&1; then
                echo "fuzz target '$target' failed:" >&2
                tail -c 4096 "$fuzz_log" >&2
                exit 1
            fi
        fi
    done
    rm -f "$fuzz_log"
elif [[ $run_fuzz -eq 1 ]]; then
    echo "fuzz smoke skipped (need nightly toolchain + 'cargo install cargo-fuzz')"
fi

# `set -e` aborts on the first failing step, so reaching here means every gate
# that ran passed. Print an explicit terminal banner: a non-zero exit already
# signals failure, but piping this script (e.g. `./check.sh | tail`) discards
# its exit code, so the banner is the in-stream success/failure signal.
echo
echo "check.sh: ALL GATES PASSED"
