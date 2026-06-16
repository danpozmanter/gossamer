"""MkDocs build hook.

Highlights ```gossamer fenced blocks with Pygments' Rust lexer.
Gossamer's surface syntax is Rust-flavoured, so the Rust lexer is a
close fit and avoids shipping a custom Pygments lexer. pymdownx
imports `get_lexer_by_name` as a module global, so aliasing it there
covers every fenced block the theme renders.
"""

from pygments.lexers.rust import RustLexer


def on_config(config):
    import pymdownx.highlight as highlight

    original = highlight.get_lexer_by_name

    def get_lexer_by_name(name, **options):
        if name in ("gossamer", "gos"):
            return RustLexer(**options)
        return original(name, **options)

    highlight.get_lexer_by_name = get_lexer_by_name
    return config
