#!/usr/bin/env bash
# End-to-end test for the Rust-binding system.
#
# Walks the entire path described in `~/dev/contexts/lang/ffi.md`:
#
# 1. Build the on-PATH `gos` binary (debug profile; release also
#    works but is slower).
# 2. Synthesise a temp project with `project.toml` declaring
#    `[rust-bindings] tuigoose = { path = ... }` and a `main.gos`
#    that imports `tuigoose::layout::rect` and dispatches it.
# 3. Run that `gos` against the project. The on-PATH binary
#    detects `[rust-bindings]`, builds a per-project runner via
#    cargo (statically linking tuigoose + gossamer-cli), and
#    `execve`s into it.
# 4. The runner's `main` calls `tuigoose::binding::force_link()`
#    + `gossamer_binding::install_all()`, then re-enters
#    `gossamer_cli::run`, which interprets `main.gos` with the
#    binding entries already installed as `Value::Native`
#    globals.
# 5. `main.gos` prints a known marker line; we grep for it on
#    stdout. Any non-zero exit, missing marker, or runner-build
#    failure is a hard fail.
#
# Designed to run from `exhaustive_test.sh` (it's slow — the
# first run does a cargo build of the runner) but it can also be
# invoked directly. Idempotent: a second run reuses the cached
# runner workspace.
#
# Environment overrides:
#   GOSSAMER_ROOT    — path to the gossamer source tree
#                      (defaults to the script's parent).
#   TUIGOOSE_ROOT    — path to the tuigoose source tree
#                      (defaults to ${GOSSAMER_ROOT}/../tuigoose).
#   GOSSAMER_CACHE   — runner-build cache root (defaults to a
#                      tempdir; the cargo target dir then lives
#                      under <cache>/runners/<fp>/target/).
#   KEEP_TEMP        — set to 1 to leave the synthesised project
#                      around after the test.

set -euo pipefail

GOSSAMER_ROOT="${GOSSAMER_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
TUIGOOSE_ROOT="${TUIGOOSE_ROOT:-$(cd "${GOSSAMER_ROOT}/../tuigoose" && pwd)}"

if [[ ! -d "${TUIGOOSE_ROOT}" ]]; then
    echo "test_rust_binding_e2e.sh: tuigoose not found at ${TUIGOOSE_ROOT}" >&2
    echo "  Set TUIGOOSE_ROOT or place tuigoose at \${GOSSAMER_ROOT}/../tuigoose." >&2
    exit 2
fi

echo "=> building gossamer-cli (debug)..."
( cd "${GOSSAMER_ROOT}" && cargo build -p gossamer-cli >/dev/null )
GOS="${GOSSAMER_ROOT}/target/debug/gos"

if [[ ! -x "${GOS}" ]]; then
    echo "test_rust_binding_e2e.sh: gos binary not produced at ${GOS}" >&2
    exit 2
fi

WORK="$(mktemp -d -t gos-binding-e2e-XXXXXX)"
if [[ -z "${KEEP_TEMP:-}" ]]; then
    trap 'rm -rf "${WORK}"' EXIT
else
    echo "test_rust_binding_e2e.sh: leaving ${WORK} for inspection"
fi

PROJECT="${WORK}/sample-app"
mkdir -p "${PROJECT}/src"

cat > "${PROJECT}/project.toml" <<EOF
[project]
id = "example.com/binding-e2e"
version = "0.1.0"

[dependencies]

[rust-bindings]
tuigoose = { path = "${TUIGOOSE_ROOT}" }
EOF

cat > "${PROJECT}/src/main.gos" <<'EOF'
use tuigoose::layout::rect
use tuigoose::layout::split_vertical
use tuigoose::block::bordered_all
use tuigoose::block::with_title
use tuigoose::paragraph::new
use tuigoose::paragraph::with_block
use tuigoose::render::paragraph_to_string
use tuigoose::render::drop_block
use tuigoose::render::drop_paragraph

fn main() {
    let quad = rect(0, 0, 12, 4)
    println("rect:", quad)

    let parts = split_vertical(quad, [2, 2])
    println("split_count:", parts)

    let block = bordered_all()
    let block = with_title(block, "hi")
    let para = new("hello world")
    let para = with_block(para, block)
    let rendered = paragraph_to_string(para, 12, 4)
    println("rendered:", rendered)

    let _ = drop_block(block)
    let _ = drop_paragraph(para)
    println("done")
}
EOF

echo "=> running gos in ${PROJECT}..."
EXTRA_CACHE_FLAGS=()
if [[ -n "${GOSSAMER_CACHE:-}" ]]; then
    export XDG_CACHE_HOME="${GOSSAMER_CACHE}"
fi

# Force the runner to build synchronously and forward output so a
# cargo failure surfaces immediately. The first invocation
# typically takes ~30-60s; cached runs are sub-second.
OUT="$( cd "${PROJECT}" && "${GOS}" run src/main.gos 2>&1 )"
STATUS=$?

echo "${OUT}"
if [[ ${STATUS} -ne 0 ]]; then
    echo "test_rust_binding_e2e.sh: gos run failed (exit ${STATUS})" >&2
    exit 1
fi

for marker in "rect:" "split_count:" "rendered:" "hello" "done" "┌hi"; do
    if ! grep -Fq "${marker}" <<<"${OUT}"; then
        echo "test_rust_binding_e2e.sh: missing marker '${marker}' on stdout" >&2
        exit 1
    fi
done

echo "=> ok (cached runner: ${PROJECT})"
