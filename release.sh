#!/usr/bin/env bash
set -euo pipefail

if [ $# -eq 0 ]; then
  echo "Usage: $0 <commit message>"
  exit 1
fi

MSG="$*"

VERSION="$(awk -F'"' '
  /^\[workspace\.package\]$/ { in_section = 1; next }
  /^\[/ { in_section = 0 }
  in_section && $1 ~ /^version = / { print $2; exit }
' Cargo.toml)"

if [ -z "$VERSION" ]; then
  echo "error: could not read version from Cargo.toml"
  exit 1
fi

TAG="v$VERSION"

run() {
  echo "+ $*"
  "$@"
}

# --- Pre-release gates -------------------------------------------------
#
# Every gate must pass cleanly before we touch git. Each step's output
# is captured to a logfile so that the on-screen summary stays compact;
# on failure (non-zero exit OR any "warning:" line in the captured
# output) we stop and dump the relevant logfile so the user can see
# exactly why the gate refused. `cargo audit` / `cargo deny` are also
# gated — vulnerabilities and license / advisory issues block the
# release in addition to compile-time problems.

LOGDIR="$(mktemp -d -t gos-release-XXXXXX)"

gate() {
  local name="$1"
  shift
  local log="$LOGDIR/$name.log"
  echo "+ release-gate: $name"
  if ! "$@" >"$log" 2>&1; then
    echo
    echo "release-gate FAILED: $name (exit $?)"
    echo "----- $log -----"
    cat "$log"
    exit 1
  fi
  # Treat any `warning:` line in the captured output as a hard stop.
  # `cargo fmt` and `cargo clippy --no-deps -- -D warnings` should not
  # surface any in green; if they do we want to know rather than ship.
  # `cargo audit` writes "warning: N vulnerabilities found" on a real
  # advisory hit even when it returns success in some configurations,
  # which is exactly what we want to flag.
  if grep -E '^(warning|error)' "$log" >/dev/null 2>&1; then
    echo
    echo "release-gate FAILED: $name surfaced warnings/errors"
    echo "----- $log -----"
    cat "$log"
    exit 1
  fi
}

ensure_tool() {
  local cmd="$1"
  local install_hint="$2"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "release-gate setup: $cmd not on PATH"
    echo "  install with: $install_hint"
    exit 1
  fi
}

ensure_tool cargo "rustup install stable"
# audit + deny are cargo subcommands shipped via separate crates.io
# packages; users without them get a precise install hint instead of
# a confusing "subcommand not found".
if ! cargo audit --version >/dev/null 2>&1; then
  echo "release-gate setup: cargo-audit not installed"
  echo "  install with: cargo install cargo-audit --locked"
  exit 1
fi
if ! cargo deny --version >/dev/null 2>&1; then
  echo "release-gate setup: cargo-deny not installed"
  echo "  install with: cargo install cargo-deny --locked"
  exit 1
fi

gate fmt        cargo fmt --all -- --check
gate clippy     cargo clippy --workspace --all-targets -- -D warnings
gate test       cargo test --workspace
gate audit      cargo audit
gate deny       cargo deny check

echo "+ release-gate: all gates green"

# --- Tag + commit ------------------------------------------------------

run git add -A
run git commit -m "$MSG"
run git tag "$TAG"

read -r -p "git push? [y/N] " push
if [[ "$push" =~ ^[Yy]$ ]]; then
  run git push
fi

read -r -p "git push origin $TAG? [y/N] " push_tag
if [[ "$push_tag" =~ ^[Yy]$ ]]; then
  run git push origin "$TAG"
fi
