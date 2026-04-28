#!/bin/sh
# Asserts the runtime stays mostly asleep when user code does nothing.
# Boots `gos run empty.gos`, which calls `time::sleep(2000)`. The
# acceptance threshold is the total user+system CPU consumed during
# those two wall-clock seconds.
#
# Threshold rationale: 200 ms locally — we measure single-digit ms
# after Track A. CI's hosted runners jitter, so the gate is bumped
# to 400 ms to absorb that without losing the regression signal
# (a return to busy-poll would burn 1500 ms+).

set -eu

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
gos="${GOS:-$repo/target/release/gos}"
threshold_ms="${IDLE_CPU_THRESHOLD_MS:-400}"

if [ ! -x "$gos" ]; then
    echo "error: gos binary not found at $gos — set GOS or build first" >&2
    exit 2
fi

TIMEFORMAT='%U %S'
# Run inside `bash -c` so we can use the POSIX `time` builtin's
# user/system breakdown; route to a tmp file so the program's stdout
# does not collide with the timing output on stderr.
out="$(mktemp)"
trap 'rm -f "$out"' EXIT

# `bash -c 'time ...'` writes user/sys/real to stderr in a portable
# format. Capture stderr only and ignore the program's stdout.
time_output="$(bash -c "
TIMEFORMAT='%U %S'
{ time '$gos' run '$here/empty.gos' >'$out'; } 2>&1
" 2>&1)"

# Parse the trailing "<user> <sys>" line emitted by bash time.
last_line="$(printf '%s\n' "$time_output" | tail -n 1)"
user_s="$(printf '%s' "$last_line" | awk '{print $1}')"
sys_s="$(printf '%s' "$last_line" | awk '{print $2}')"

if [ -z "$user_s" ] || [ -z "$sys_s" ]; then
    printf 'error: could not parse bash time output:\n%s\n' "$time_output" >&2
    exit 2
fi

# Convert seconds-with-millis to integer milliseconds via awk.
total_ms="$(awk -v u="$user_s" -v s="$sys_s" 'BEGIN { printf "%d", (u + s) * 1000 }')"

printf 'idle empty.gos: user=%ss sys=%ss total=%sms (threshold=%sms)\n' \
    "$user_s" "$sys_s" "$total_ms" "$threshold_ms"

if [ "$total_ms" -gt "$threshold_ms" ]; then
    echo "FAIL: idle CPU above threshold — runtime is busy-polling somewhere" >&2
    exit 1
fi
echo "ok"
