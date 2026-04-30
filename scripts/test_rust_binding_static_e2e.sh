#!/usr/bin/env bash
# Compiled-mode (gos build) e2e for the Rust-binding system.
#
# Builds each example with `gos build`, runs the produced
# binary in a scrubbed environment with no Rust toolchain on
# PATH, and asserts the same stdout markers that the runner
# (`gos run`) produces.
#
# Compiled binaries link `libgos_static_bindings.a` plus the
# gossamer runtime, and the cranelift codegen lowers binding
# call sites to direct `gos_binding_*` C-ABI calls. The whole
# pipeline is supposed to work without `cargo` / `rustc` on
# PATH at runtime.

set -euo pipefail

GOSSAMER_ROOT="${GOSSAMER_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"

echo "=> building gos..."
( cd "${GOSSAMER_ROOT}" && cargo build -p gossamer-cli >/dev/null )
GOS="${GOSSAMER_ROOT}/target/debug/gos"

if [[ ! -x "${GOS}" ]]; then
    echo "test_rust_binding_static_e2e.sh: gos binary not produced at ${GOS}" >&2
    exit 2
fi

CACHE="$(mktemp -d -t gos-static-cache-XXXXXX)"
trap 'rm -rf "${CACHE}"' EXIT
export XDG_CACHE_HOME="${CACHE}"

build_and_run() {
    local label="$1"
    local proj="$2"
    local entry="$3"
    shift 3
    local expected_markers=("$@")

    echo "=> ${label}"
    cd "${proj}"
    rm -rf target
    "${GOS}" build "${entry}"

    local binary
    binary="$(ls target/debug/* 2>/dev/null | head -n1 || true)"
    if [[ ! -x "${binary}" ]]; then
        echo "test_rust_binding_static_e2e.sh: build did not produce a binary in ${proj}/target/debug" >&2
        exit 1
    fi

    # Scrubbed environment: only /usr/bin and /bin on PATH.
    # Asserts the binary doesn't depend on cargo / rustc /
    # rustup at runtime.
    local out
    out="$( env -i PATH=/usr/bin:/bin "${binary}" 2>&1 )"
    echo "${out}"
    for m in "${expected_markers[@]}"; do
        if ! grep -Fq "${m}" <<<"${out}"; then
            echo "test_rust_binding_static_e2e.sh: missing marker '${m}' for ${label}" >&2
            exit 1
        fi
    done
    cd "${GOSSAMER_ROOT}"
}

build_and_run \
    "example 01 (echo-binding) — gos build" \
    "${GOSSAMER_ROOT}/example-external-libraries/01-gossamer-aware" \
    src/main.gos \
    "HELLO, GOSSAMER" "sum: 15" "count: 3"

build_and_run \
    "example 02 (unic-segment wrapper) — gos build" \
    "${GOSSAMER_ROOT}/example-external-libraries/02-plain-rust-wrapped" \
    src/main.gos \
    "count: 10" "grapheme: n" "grapheme: ï"

echo "=> static-link e2e green"
