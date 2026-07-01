#!/bin/sh
# Cross-build the canonical fixture list for a Linux target.
# Usage: scripts/cross_build_fixtures.sh <target-triple> <out-base>
#
# Binaries land in <out-base>/<target-triple>/ so several targets can
# share one upload artifact without filename collisions. Used by the
# macOS / Windows host cross CI jobs: they produce the binaries here,
# upload them, and a Linux job runs them (natively for x86_64, under
# QEMU for aarch64) and diffs against the bytecode VM
# (`scripts/qemu_diff_against_vm.sh`). The runtime archive is supplied
# via the `GOS_RUNTIME_LIB_<TRIPLE>` environment variable set by the
# calling job.
set -eu

TRIPLE="$1"
OUT_BASE="$2"
GOS="${GOS_BIN:-./target/debug/gos}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$OUT_BASE/$TRIPLE"

mkdir -p "$OUT"
while IFS= read -r src; do
    [ -n "$src" ] || continue
    case "$src" in \#*) continue ;; esac
    echo "cross-build $src -> $TRIPLE"
    "$GOS" build --release --target "$TRIPLE" "$ROOT/$src" --out-dir "$OUT"
done < "$ROOT/scripts/cross_fixtures.txt"
