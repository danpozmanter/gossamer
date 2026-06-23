"""MkDocs build hook.

Highlights ```gossamer fenced blocks with Pygments' Rust lexer.
Gossamer's surface syntax is Rust-flavoured, so the Rust lexer is a
close fit and avoids shipping a custom Pygments lexer. pymdownx
imports `get_lexer_by_name` as a module global, so aliasing it there
covers every fenced block the theme renders.

Also patches the version tag in landing/index.html to match the
workspace version in Cargo.toml so the two never drift.
"""

import os
import re

from pygments.lexers.rust import RustLexer


def _workspace_version(config_file_path: str) -> str:
    """Read the workspace version from Cargo.toml next to mkdocs.yml."""
    root = os.path.dirname(os.path.abspath(config_file_path))
    cargo = os.path.join(root, "Cargo.toml")
    text = open(cargo, encoding="utf-8").read()
    m = re.search(r'^version\s*=\s*"([^"]+)"', text, re.MULTILINE)
    return m.group(1) if m else ""


def _patch_landing_version(config_file_path: str, version: str) -> None:
    """Replace the <span class="ver-tag"> version in landing/index.html."""
    if not version:
        return
    root = os.path.dirname(os.path.abspath(config_file_path))
    landing = os.path.join(root, "landing", "index.html")
    if not os.path.exists(landing):
        return
    original = open(landing, encoding="utf-8").read()
    patched = re.sub(
        r'(<span class="ver-tag">v)[^<]+(</span>)',
        rf'\g<1>{version}\2',
        original,
    )
    if patched != original:
        open(landing, "w", encoding="utf-8").write(patched)


def on_config(config):
    import pymdownx.highlight as highlight

    original = highlight.get_lexer_by_name

    def get_lexer_by_name(name, **options):
        if name in ("gossamer", "gos"):
            return RustLexer(**options)
        return original(name, **options)

    highlight.get_lexer_by_name = get_lexer_by_name

    version = _workspace_version(config["config_file_path"])
    _patch_landing_version(config["config_file_path"], version)

    return config
