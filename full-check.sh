#!/usr/bin/env bash
# Complete local validation gate. Runs every quick validation first, then the
# full workspace test suite serially so shared external resources do not make
# tests flaky.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

for arg in "$@"; do
    if [[ "$arg" == "-h" || "$arg" == "--help" ]]; then
        "$script_dir/quick-check.sh" --help
        echo
        echo "full-check.sh accepts the same flags, then runs cargo test --workspace."
        exit 0
    fi
done

"$script_dir/quick-check.sh" "$@"

echo
echo "-- full workspace test gate --"
cargo test --doc --workspace --release
cargo test --workspace --no-fail-fast -- --test-threads=1

echo
echo "full-check.sh: ALL GATES PASSED"
