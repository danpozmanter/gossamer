#!/bin/sh
# Gossamer installer for Linux and macOS.
#
# Two modes:
#   --user    install into ~/.local/bin (no sudo needed; default)
#   --system  install into /usr/local/bin (may prompt for sudo)
#
# The script works both as a local installer (run from a release
# archive after extracting it) and as a remote bootstrap:
#
#     curl -fsSL https://raw.githubusercontent.com/gossamer-lang/gossamer/main/scripts/install.sh | sh
#     curl -fsSL https://raw.githubusercontent.com/gossamer-lang/gossamer/main/scripts/install.sh | sh -s -- --system
#
# Honoured environment variables:
#   GOSSAMER_VERSION  release tag to install (default: "latest")
#   GOSSAMER_REPO     github owner/repo     (default: "gossamer-lang/gossamer")
#   GOSSAMER_PREFIX   install root          (overrides --user / --system)

set -eu

REPO="${GOSSAMER_REPO:-gossamer-lang/gossamer}"
VERSION="${GOSSAMER_VERSION:-latest}"

MODE="user"
for arg in "$@"; do
    case "$arg" in
        --user)    MODE="user" ;;
        --system)  MODE="system" ;;
        -h|--help)
            sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "gossamer-install: unknown argument: $arg" >&2
            exit 2
            ;;
    esac
done

if [ -n "${GOSSAMER_PREFIX:-}" ]; then
    BIN_DIR="$GOSSAMER_PREFIX/bin"
elif [ "$MODE" = "system" ]; then
    BIN_DIR="/usr/local/bin"
else
    BIN_DIR="$HOME/.local/bin"
fi

uname_s=$(uname -s 2>/dev/null || echo unknown)
uname_m=$(uname -m 2>/dev/null || echo unknown)

case "$uname_s" in
    Linux)   os="linux" ;;
    Darwin)  os="macos" ;;
    *)
        echo "gossamer-install: unsupported host OS: $uname_s" >&2
        echo "                  (Windows users: run the .exe installer)" >&2
        exit 1
        ;;
esac

case "$uname_m" in
    x86_64|amd64)      arch="x86_64" ;;
    arm64|aarch64)     arch="aarch64" ;;
    *)
        echo "gossamer-install: unsupported host architecture: $uname_m" >&2
        exit 1
        ;;
esac

install_file() {
    src="$1"
    dest="$2"
    if [ -w "$(dirname "$dest")" ]; then
        cp "$src" "$dest"
        chmod 755 "$dest"
    elif command -v sudo >/dev/null 2>&1; then
        sudo cp "$src" "$dest"
        sudo chmod 755 "$dest"
    else
        echo "gossamer-install: cannot write to $dest and sudo not available" >&2
        exit 1
    fi
}

ensure_dir() {
    dir="$1"
    if [ -d "$dir" ]; then
        return
    fi
    # Find the deepest existing ancestor and see if we can write there.
    probe="$dir"
    while [ ! -d "$probe" ]; do
        probe="$(dirname "$probe")"
    done
    if [ -w "$probe" ]; then
        mkdir -p "$dir"
    elif command -v sudo >/dev/null 2>&1; then
        sudo mkdir -p "$dir"
    else
        echo "gossamer-install: cannot create $dir and sudo not available" >&2
        exit 1
    fi
}

# Path 1 — local install from an already-extracted archive.
script_dir="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
if [ -f "$script_dir/gos" ]; then
    ensure_dir "$BIN_DIR"
    install_file "$script_dir/gos" "$BIN_DIR/gos"
    printf 'Installed gos to %s\n' "$BIN_DIR/gos"
    "$BIN_DIR/gos" --version 2>/dev/null || true
    exit 0
fi

# Path 2 — remote bootstrap: download the release asset.
if ! command -v curl >/dev/null 2>&1; then
    echo "gossamer-install: curl is required for remote installs" >&2
    exit 1
fi

if [ "$VERSION" = "latest" ]; then
    VERSION_URL="https://api.github.com/repos/$REPO/releases/latest"
    TAG=$(curl -fsSL "$VERSION_URL" | sed -n 's/^[[:space:]]*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)
    if [ -z "$TAG" ]; then
        echo "gossamer-install: could not read latest release tag from $VERSION_URL" >&2
        exit 1
    fi
else
    TAG="$VERSION"
fi

# Strip leading `v` on the tag for filename matching.
VER_NUM=$(printf '%s' "$TAG" | sed 's/^v//')

case "$os" in
    linux) ASSET="gos-${VER_NUM}-linux-${arch}.tar.gz" ;;
    macos) ASSET="gos-${VER_NUM}-macos-${arch}.zip" ;;
esac
URL="https://github.com/$REPO/releases/download/$TAG/$ASSET"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

printf 'Downloading %s ...\n' "$URL"
curl -fL -o "$tmp/$ASSET" "$URL"

cd "$tmp"
case "$ASSET" in
    *.tar.gz) tar xzf "$ASSET" ;;
    *.zip)
        if command -v unzip >/dev/null 2>&1; then
            unzip -q "$ASSET"
        else
            echo "gossamer-install: unzip is required to extract $ASSET" >&2
            exit 1
        fi
        ;;
esac

extracted_dir="$(find . -maxdepth 1 -mindepth 1 -type d | head -n1)"
if [ -z "$extracted_dir" ] || [ ! -f "$extracted_dir/gos" ]; then
    echo "gossamer-install: extracted archive layout unexpected" >&2
    exit 1
fi

ensure_dir "$BIN_DIR"
install_file "$extracted_dir/gos" "$BIN_DIR/gos"

printf 'Installed gos %s to %s\n' "$TAG" "$BIN_DIR/gos"
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        printf '\nNote: %s is not on your PATH.\n' "$BIN_DIR"
        printf 'Add the following line to your shell profile to use `gos`:\n'
        printf '    export PATH="%s:$PATH"\n' "$BIN_DIR"
        ;;
esac

"$BIN_DIR/gos" --version 2>/dev/null || true
