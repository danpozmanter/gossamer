#!/usr/bin/env bash
# Runs every test that gates on `GOSSAMER_TESTS_FULL` against its
# full corpus. The routine `cargo test` run samples each suite so
# the workspace test stays under a minute; this script is for
# pre-release verification when you want every program / example
# covered.
#
# Currently full-mode-aware suites:
#
# - `gossamer-codegen-cranelift::codegen_correct` — runs every
#   `.gos` program in `tests/correct/` (~100) through three tiers
#   (`gos run`, `gos build`, `gos build --release`) and diffs
#   stdout against the sibling `.expected` file.
# - `gossamer-cli::parity::every_example_runs_cleanly_through_the_interpreter`
#   — every file in `examples/*.gos` must execute under
#   `gos run` with exit 0.
# - `gossamer-cli::parity::interpreter_and_native_paths_agree_on_every_example`
#   — every example must build natively and produce
#   byte-identical stdout / stderr / exit code under both tiers.
# - `gossamer-interp::perf_baseline::bench_*` — the
#   "great-leap-forward" micro-benchmark suite. Default `cargo
#   test` runs each at a smoke-test loop bound; FULL mode scales
#   loop counts back up by 1000× so the printed timings match
#   the historical numbers.
#
# Each is also runnable in isolation with `GOSSAMER_TESTS_FULL=1
# cargo test -p <crate> --test <name>`. Forwarded arguments are
# passed straight through to `cargo test` (e.g.
# `./exhaustive_test.sh -- --test-threads=1`).

set -euo pipefail

cd "$(dirname "$0")"

GOSSAMER_TESTS_FULL=1 cargo test \
    -p gossamer-codegen-cranelift \
    --test codegen_correct \
    --release \
    -- \
    --nocapture \
    "$@"

GOSSAMER_TESTS_FULL=1 cargo test \
    -p gossamer-cli \
    --test parity \
    --features exhaustive_tests \
    --release \
    -- \
    --nocapture \
    "$@"

GOSSAMER_TESTS_FULL=1 cargo test \
    -p gossamer-interp \
    --test perf_baseline \
    --release \
    -- \
    --nocapture \
    "$@"
