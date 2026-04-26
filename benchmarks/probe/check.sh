#!/bin/sh
# Re-run the codegen probes and diff against the committed baseline.
# Exit non-zero on any mismatch so CI catches accidental regressions
# or silent widenings.

set -eu

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
gos="${GOS:-$repo/target/release/gos}"

if [ ! -x "$gos" ]; then
    echo "error: gos binary not found at $gos — set GOS or build first" >&2
    exit 2
fi

cd "$here"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

for p in p*.gos; do
    stem="${p%.gos}"
    rm -f "$stem"
    # Post-L4 every probe either builds native or fails outright —
    # no launcher fallback.
    stderr="$($gos build "$p" 2>&1 1>/dev/null || true)"
    if [ -x "$stem" ]; then
        printf '%-28s  native      ok\n' "$p" >> "$tmp"
    else
        short=$(echo "$stderr" | head -3 | tr '\n' ' ')
        printf '%-28s  failed      %s\n' "$p" "$short" >> "$tmp"
    fi
    rm -f "$stem"
done

if diff -u results.txt "$tmp" >&2; then
    echo "probe matrix unchanged"
else
    echo "probe matrix drift — update results.txt if intentional" >&2
    exit 1
fi
