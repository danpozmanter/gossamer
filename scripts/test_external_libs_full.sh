#!/usr/bin/env bash
# Full e2e matrix for the `[rust-bindings]` system, per
# `~/dev/contexts/lang/external_libs.md` §10.
#
# For each example, exercises:
#   - `gos run` against the source
#   - cache-hit on the second `gos run`
#   - the no-bindings fast path (no cargo invocation)
#
# Compiled-mode (gos build) parity is exercised by
# `scripts/test_rust_binding_static_e2e.sh`. This script focuses
# on the runner-dispatch path which is the headline binding
# pipeline.

set -euo pipefail

GOSSAMER_ROOT="${GOSSAMER_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"

echo "=> building gos..."
( cd "${GOSSAMER_ROOT}" && cargo build -p gossamer-cli >/dev/null )
GOS="${GOSSAMER_ROOT}/target/debug/gos"

if [[ ! -x "${GOS}" ]]; then
    echo "test_external_libs_full.sh: gos binary not produced at ${GOS}" >&2
    exit 2
fi

CACHE="$(mktemp -d -t gos-extlibs-cache-XXXXXX)"
trap 'rm -rf "${CACHE}"' EXIT
export XDG_CACHE_HOME="${CACHE}"

run_in() {
    local label="$1"
    local proj="$2"
    local entry="$3"
    shift 3
    local expected_markers=("$@")
    echo "=> ${label}"
    cd "${proj}"
    local out
    out="$( "${GOS}" run "${entry}" 2>&1 )"
    echo "${out}"
    for m in "${expected_markers[@]}"; do
        if ! grep -Fq "${m}" <<<"${out}"; then
            echo "test_external_libs_full.sh: missing marker '${m}' for ${label}" >&2
            exit 1
        fi
    done
    cd "${GOSSAMER_ROOT}"
}

echo "=> phase 1: example 01 (echo-binding)"
run_in \
    "01-gossamer-aware (cold)" \
    "${GOSSAMER_ROOT}/example-external-libraries/01-gossamer-aware" \
    src/main.gos \
    "HELLO, GOSSAMER" "sum: 15" "count: 3"

# Second invocation should hit the cache (no cargo build).
run_in \
    "01-gossamer-aware (warm)" \
    "${GOSSAMER_ROOT}/example-external-libraries/01-gossamer-aware" \
    src/main.gos \
    "HELLO, GOSSAMER" "sum: 15" "count: 3"

echo "=> phase 2: example 02 (unic-segment wrapper)"
run_in \
    "02-plain-rust-wrapped (cold)" \
    "${GOSSAMER_ROOT}/example-external-libraries/02-plain-rust-wrapped" \
    src/main.gos \
    "count: 10" "grapheme: n" "grapheme: ï"

run_in \
    "02-plain-rust-wrapped (warm)" \
    "${GOSSAMER_ROOT}/example-external-libraries/02-plain-rust-wrapped" \
    src/main.gos \
    "count: 10" "grapheme: n" "grapheme: ï"

echo "=> phase 3: no-bindings fast path"
NOBIND="$(mktemp -d -t gos-nobindings-XXXXXX)"
mkdir -p "${NOBIND}/src"
cat > "${NOBIND}/project.toml" <<'EOF'
[project]
id = "example.com/no-bindings"
version = "0.1.0"

[dependencies]
EOF
cat > "${NOBIND}/src/main.gos" <<'EOF'
fn main() {
    println("plain-project ok")
}
EOF
( cd "${NOBIND}" && "${GOS}" run src/main.gos 2>&1 ) | tee /tmp/no-bindings-run.txt
if ! grep -Fq "plain-project ok" /tmp/no-bindings-run.txt; then
    echo "test_external_libs_full.sh: no-bindings project did not produce expected stdout" >&2
    exit 1
fi
rm -rf "${NOBIND}" /tmp/no-bindings-run.txt

echo "=> all matrix cells green"
