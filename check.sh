#!/usr/bin/env bash
# Pre-commit gate: fmt + clippy + tests + stdlib docs drift +
# fuzz smoke. By default each step's chatter is suppressed and only
# warnings, errors, and the step summary surface; pass --full to see
# every line. Any step's non-zero exit replays the captured output
# before bailing so the failure is debuggable.
set -euo pipefail

full=0
for arg in "$@"; do
    case "$arg" in
        --full) full=1 ;;
        -h|--help)
            echo "usage: $0 [--full]"
            echo "  --full   show every step's full output (default: warnings + errors only)"
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
            echo "$label FAILED — full output:" >&2
            cat "$log" >&2
            rm -f "$log"
            exit 1
        fi
        # Surface warning / error lines (plus a couple of lines of
        # following context — diagnostics usually print the source
        # excerpt right after the header) so problems aren't silent
        # even when the step succeeds with warnings.
        grep -E -i -A 2 '^(warning|error)[:\[]|: warning:|: error:' "$log" || true
    fi
    rm -f "$log"
}

run_step "cargo fmt"                                       cargo fmt
run_step "cargo clippy --workspace --all-targets"          cargo clippy --workspace --all-targets -- -D warnings
run_step "cargo test --workspace --no-fail-fast"           cargo test --workspace --no-fail-fast
# Stdlib docs drift gate — verifies docs_src/stdlib/ pages match
# what `manifest::ALL_MODULES` would emit. Build the binary first
# so the check uses the freshly built crate.
run_step "cargo build --bin gos"                           cargo build --bin gos
run_step "gos doc --emit-stdlib --check"                   ./target/debug/gos doc --emit-stdlib docs_src/stdlib --check

# Fuzz smoke — mirrors `.github/workflows/fuzz.yml` so adversarial
# inputs that CI would flag also fail locally. Each target runs
# briefly (10 s by default; override with GOSSAMER_FUZZ_SECS) and
# replays its seed corpus. Skip cleanly when cargo-fuzz or the
# nightly toolchain isn't installed so the rest of `check.sh`
# stays useful on dev machines that haven't set the harness up.
fuzz_secs="${GOSSAMER_FUZZ_SECS:-10}"
if command -v cargo-fuzz >/dev/null 2>&1 && rustup toolchain list 2>/dev/null | grep -q '^nightly'; then
    echo "==> fuzz smoke (${fuzz_secs}s per target)"
    fuzz_log="$(mktemp -d)/fuzz.log"
    for target in lex parse manifest http_request typecheck resolve mir_lower hir_lower vm_compile vm_run; do
        if [[ $full -eq 1 ]]; then
            echo "  -> $target"
            if ! ( cd fuzz && cargo +nightly fuzz run "$target" -- \
                    -max_total_time="$fuzz_secs" -max_len=65536 ); then
                exit 1
            fi
        else
            if ! ( cd fuzz && cargo +nightly fuzz run "$target" -- \
                    -max_total_time="$fuzz_secs" -max_len=65536 ) \
                    >"$fuzz_log" 2>&1; then
                echo "fuzz target '$target' failed:" >&2
                tail -c 4096 "$fuzz_log" >&2
                exit 1
            fi
        fi
    done
    rm -f "$fuzz_log"
else
    echo "fuzz smoke skipped (need nightly toolchain + 'cargo install cargo-fuzz')"
fi
