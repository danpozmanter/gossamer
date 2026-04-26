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
