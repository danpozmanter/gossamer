#!/bin/sh
# Run cross-built Linux binaries and assert each produces output
# bit-identical to the bytecode VM (`gos run`) for its source. Used by
# the Linux job that consumes the macOS/Windows host cross artifacts.
# Usage: scripts/qemu_diff_against_vm.sh <bins-root>
#   Layout: <bins-root>/<artifact>/<target-triple>/<stem>
#   (artifact = e.g. cross-bins-macos; produced by cross_build_fixtures).
# An x86_64 binary runs natively on an x86_64 host; an aarch64 binary
# runs under qemu-aarch64.
set -eu

BINS_ROOT="$1"
GOS="${GOS_BIN:-./target/debug/gos}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HOST_ARCH="$(uname -m)"

runner_for() {
    # echo the command prefix to run a binary for target arch $1, or
    # "SKIP" when no runner is available on this host.
    case "$1" in
        "$HOST_ARCH") echo "" ;;
        aarch64) command -v qemu-aarch64 >/dev/null 2>&1 && echo "qemu-aarch64" || echo "SKIP" ;;
        x86_64)  command -v qemu-x86_64  >/dev/null 2>&1 && echo "qemu-x86_64"  || echo "SKIP" ;;
        *) echo "SKIP" ;;
    esac
}

rc=0
for artifact in "$BINS_ROOT"/*/; do
    [ -d "$artifact" ] || continue
    for tdir in "$artifact"*/; do
        [ -d "$tdir" ] || continue
        triple="$(basename "$tdir")"
        arch="${triple%%-*}"
        runner="$(runner_for "$arch")"
        if [ "$runner" = "SKIP" ]; then
            echo "::notice::skip $triple (no runner for $arch on $HOST_ARCH)"
            continue
        fi
        while IFS= read -r src; do
            [ -n "$src" ] || continue
            case "$src" in \#*) continue ;; esac
            stem="$(basename "$src" .gos)"
            bin="$tdir$stem"
            # actions/upload-artifact and download-artifact do not preserve
            # Unix file permissions - every downloaded file lands as 644
            # regardless of what it was uploaded as - so the executable bit
            # set at cross-build time never survives the round trip and
            # must be restored before running.
            [ -f "$bin" ] && chmod +x "$bin"
            [ -x "$bin" ] || { echo "::error::missing binary $bin"; rc=1; continue; }
            vm_out="$("$GOS" run "$ROOT/$src")"
            if [ -z "$runner" ]; then
                run_out="$("$bin")"
            else
                run_out="$("$runner" "$bin")"
            fi
            if [ "$vm_out" != "$run_out" ]; then
                echo "::error::$(basename "$artifact"): $triple $src cross output != VM"
                printf 'VM:\n%s\nRUN:\n%s\n' "$vm_out" "$run_out" >&2
                rc=1
            else
                echo "ok   $(basename "$artifact"): $triple $src"
            fi
        done < "$ROOT/scripts/cross_fixtures.txt"
    done
done
exit "$rc"
