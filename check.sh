#!/usr/bin/env bash
set -euo pipefail

cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
# Stdlib docs drift gate — verifies docs_src/stdlib/ pages match
# what `manifest::ALL_MODULES` would emit. Build the binary first
# so the check uses the freshly built crate.
cargo build --bin gos
./target/debug/gos doc --emit-stdlib docs_src/stdlib --check

# Fuzz smoke — mirrors `.github/workflows/fuzz.yml` so adversarial
# inputs that CI would flag also fail locally. Each target runs
# briefly (10 s by default; override with GOSSAMER_FUZZ_SECS) and
# replays its seed corpus. Skip cleanly when cargo-fuzz or the
# nightly toolchain isn't installed so the rest of `check.sh`
# stays useful on dev machines that haven't set the harness up.
fuzz_secs="${GOSSAMER_FUZZ_SECS:-10}"
if command -v cargo-fuzz >/dev/null 2>&1 && rustup toolchain list 2>/dev/null | grep -q '^nightly'; then
    echo "fuzz smoke (${fuzz_secs}s per target)"
    fuzz_log="$(mktemp -d)/fuzz.log"
    for target in lex parse manifest http_request typecheck mir_lower vm_compile; do
        echo "  -> $target"
        if ! ( cd fuzz && cargo +nightly fuzz run "$target" -- \
                -max_total_time="$fuzz_secs" -max_len=65536 ) \
                >"$fuzz_log" 2>&1; then
            echo "fuzz target '$target' failed:"
            tail -c 4096 "$fuzz_log"
            exit 1
        fi
    done
    rm -f "$fuzz_log"
else
    echo "fuzz smoke skipped (need nightly toolchain + 'cargo install cargo-fuzz')"
fi
