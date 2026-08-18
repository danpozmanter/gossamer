#!/usr/bin/env bash
# VM-vs-JIT parity sweep over every fixture, in parallel.
#
# Runs each `.gos` under `examples/` and `feature-testing-examples/` twice
# through `gos run`: once with the JIT off, once with it on. A promoted body
# that answers differently - or crashes - is a JIT bug, and this catches it in
# seconds where the full tier-parity walk (which also builds each fixture
# natively) takes tens of minutes.
#
# Usage: scripts/jit_parity_sweep.sh [--force-jit] [gos-binary]
#
#   --force-jit   promote every body at its first call instead of at the
#                 shipped hotness threshold, which widens coverage well past
#                 what a short fixture reaches on its own - and slows the
#                 sweep down by roughly an order of magnitude.
#
# Output is compared as a line multiset rather than a transcript: a program
# whose goroutines interleave differently between two runs of the same binary
# would otherwise read as a divergence here. Every value, count, and crash
# still has to match; exact line order is what the full tier-parity walk
# compares.
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
force_jit=0
gos=""
for arg in "$@"; do
    case "$arg" in
        --force-jit) force_jit=1 ;;
        *) gos="$arg" ;;
    esac
done
gos="${gos:-$root/target/debug/gos}"
if [[ ! -x "$gos" ]]; then
    echo "jit parity sweep: no gos binary at $gos (cargo build --bin gos)" >&2
    exit 2
fi

out_dir="$(mktemp -d)"
trap 'rm -rf "$out_dir"' EXIT

compare_one() {
    local file="$1"
    local rel="${file#"$root"/}"
    local vm jit vm_code jit_code
    vm="$(cd "$root" && GOS_JIT=0 timeout 30 "$gos" run "$rel" 2>/dev/null)"
    vm_code=$?
    if [[ $force_jit -eq 1 ]]; then
        jit="$(cd "$root" && GOSSAMER_JIT_THRESHOLD=1 timeout 30 "$gos" run "$rel" 2>/dev/null)"
    else
        jit="$(cd "$root" && timeout 30 "$gos" run "$rel" 2>/dev/null)"
    fi
    jit_code=$?
    if [[ "$(printf '%s' "$vm" | sort)" != "$(printf '%s' "$jit" | sort)" \
        || "$vm_code" != "$jit_code" ]]; then
        {
            echo "=== $rel"
            echo "  vm  exit=$vm_code"
            echo "  jit exit=$jit_code"
            diff <(printf '%s' "$vm" | sort) <(printf '%s' "$jit" | sort) | head -20
        } >>"$out_dir/failures"
    fi
}
export -f compare_one
export root gos out_dir force_jit

# `examples/projects/` holds whole project trees - servers and other
# long-running entry points the parity walk does not run either - so only the
# single-file fixtures directly under each root are swept.
find "$root/examples" "$root/feature-testing-examples" -maxdepth 1 -name '*.gos' -print0 \
    | xargs -0 -P "$(nproc)" -I{} bash -c 'compare_one "$@"' _ {}

if [[ -s "$out_dir/failures" ]]; then
    echo "JIT-vs-VM divergences:" >&2
    cat "$out_dir/failures" >&2
    exit 1
fi
echo "jit parity sweep: every fixture agrees between the VM and the JIT"
