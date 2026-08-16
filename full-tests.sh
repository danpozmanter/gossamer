#!/usr/bin/env bash
set -euo pipefail

echo "-- full workspace test gate --"
cargo test --doc --workspace --release
cargo test --workspace --no-fail-fast -- --test-threads=1
