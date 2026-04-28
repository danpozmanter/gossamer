#!/usr/bin/env bash
# Joint Track A + B load validation. See README.md for the
# assertions this script enforces.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$ROOT/../.." && pwd)"
SERVICE_DIR="$REPO_ROOT/examples/projects/web_service_full"

CONNECTIONS=${CONNECTIONS:-1000}
DURATION_SEC=${DURATION_SEC:-60}
SOAK=0
USE_VEGETA=0
EMIT_METRICS=0

while (( "$#" )); do
    case "$1" in
        --connections=*) CONNECTIONS="${1#*=}"; shift ;;
        --duration=*) DURATION_SEC="${1#*=}"; shift ;;
        --soak) SOAK=1; DURATION_SEC=1800; shift ;;
        --vegeta) USE_VEGETA=1; shift ;;
        --metrics) EMIT_METRICS=1; shift ;;
        --help|-h)
            cat <<USAGE
Usage: $0 [--connections=N] [--duration=SEC] [--soak] [--vegeta] [--metrics]

Defaults: 1000 connections, 60-second window, bundled harness.
USAGE
            exit 0 ;;
        *) echo "unknown arg: $1" >&2; exit 64 ;;
    esac
done

PORT=${PORT:-8443}
GOMAXPROCS_FOR_SVC=${GOMAXPROCS_FOR_SVC:-$(nproc 2>/dev/null || echo 4)}
LOG_DIR="$(mktemp -d -t gos-bench-XXXXXX)"
trap 'rm -rf "$LOG_DIR"' EXIT

echo "[bench] log dir: $LOG_DIR"
echo "[bench] connections=$CONNECTIONS duration=${DURATION_SEC}s GOMAXPROCS=$GOMAXPROCS_FOR_SVC"

# Build the service in release mode so DWARF + write barriers are
# exercised end-to-end.
GOSSAMER_PROCS="$GOMAXPROCS_FOR_SVC" \
    "$REPO_ROOT/target/release/gos" build --release "$SERVICE_DIR/src/main.gos" 2>&1 \
    | tee "$LOG_DIR/build.log"

# Start the service.
SVC_BIN="${SERVICE_DIR}/main"
if [[ ! -x "$SVC_BIN" ]]; then
    SVC_BIN="$SERVICE_DIR/src/main"
fi
"$SVC_BIN" --port "$PORT" >"$LOG_DIR/svc.stdout" 2>"$LOG_DIR/svc.stderr" &
SVC_PID=$!
trap 'kill $SVC_PID 2>/dev/null || true; rm -rf "$LOG_DIR"' EXIT

# Wait for the listener.
for _ in $(seq 1 50); do
    if ss -tlnp 2>/dev/null | grep -q ":$PORT "; then
        break
    fi
    sleep 0.1
done

# Drive load.
START_THREADS=$(ps -o nlwp= -p "$SVC_PID" | tr -d ' ')
echo "[bench] start: pid=$SVC_PID threads=$START_THREADS"

if (( USE_VEGETA == 1 )) && command -v vegeta >/dev/null 2>&1; then
    cat <<TARGETS | vegeta attack -duration="${DURATION_SEC}s" -rate=0 -workers="$CONNECTIONS" -insecure | vegeta report
GET https://127.0.0.1:$PORT/notes
TARGETS
else
    echo "[bench] running bundled harness"
    "$REPO_ROOT/target/release/gos" run \
        "$ROOT/harness.gos" \
        -- --port "$PORT" --connections "$CONNECTIONS" --duration "$DURATION_SEC"
fi

PEAK_THREADS=$(ps -o nlwp= -p "$SVC_PID" | tr -d ' ')

# Scrape final state.
GO_COUNT=$(curl -sk "https://127.0.0.1:$PORT/debug/metrics" | awk -F'=' '/^goroutines=/ {print $2}' || echo 0)
GC_P99_MS=$(curl -sk "https://127.0.0.1:$PORT/debug/metrics" | awk -F'=' '/^gc_pause_p99_ms=/ {print $2}' || echo 0)

if (( EMIT_METRICS == 1 )); then
    curl -sk "https://127.0.0.1:$PORT/debug/metrics"
fi

# Assertions.
fail=0
THREAD_LIMIT=$(( GOMAXPROCS_FOR_SVC * 2 + 4 ))
if (( PEAK_THREADS > THREAD_LIMIT )); then
    echo "[FAIL] thread count $PEAK_THREADS > limit $THREAD_LIMIT" >&2
    fail=1
else
    echo "[ok]   threads=$PEAK_THREADS within $THREAD_LIMIT"
fi

GC_P99_INT=${GC_P99_MS%.*}
GC_P99_INT=${GC_P99_INT:-0}
if (( GC_P99_INT > 10 )); then
    echo "[FAIL] GC pause p99 ${GC_P99_MS} ms > 10 ms" >&2
    fail=1
else
    echo "[ok]   gc_pause_p99=${GC_P99_MS}ms"
fi

if (( SOAK == 1 )); then
    if (( GO_COUNT > 5000 )); then
        echo "[FAIL] goroutine count after 30-min soak: $GO_COUNT" >&2
        fail=1
    else
        echo "[ok]   goroutines after soak=$GO_COUNT"
    fi
fi

kill "$SVC_PID" 2>/dev/null || true
wait "$SVC_PID" 2>/dev/null || true

exit $fail
