# `lang::triple_quoted_string`

`"""` string literal whose body is dedented by the indentation it shares with its closing delimiter; `gos fmt` moves the block with the line that opens it.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

A triple-quoted literal writes multi-line text at the indentation of the
code around it. The body starts on the line after the opening `"""`, and
the indentation it shares with the closing `"""` is stripped from every
line.

```gossamer
fn page() -> String {
    """
    <html>
        <body>
            <h1>Hello</h1>
        </body>
    </html>
    """
}
```

`page()` answers five lines with `<html>` at column zero and `<h1>`
indented eight spaces. The four spaces every line shares with the
closing delimiter are layout, not content.

## Why

An ordinary string literal already spans lines, but its indentation is
part of its value, so embedding HTML, SQL, or JSON means un-indenting
the body to column zero and losing the shape of the enclosing function:

```gossamer
let text = "<html>
    <body>
</html>"
```

The triple-quoted form keeps the block where it belongs and lets `gos
fmt` move it as a unit.

## Rules

- **The opening line carries only the delimiter.** Whitespace may follow
  the opening `"""`; anything else is `GP0033`. The newline after the
  delimiter is not part of the value.
- **The closing line sets the measure.** When the closing `"""` sits on
  a line of its own, that line contributes its whitespace to the
  indentation measure and the newline before it is not part of the
  value. Put content immediately before the closing delimiter and the
  last line simply has no trailing newline.
- **The measure is the shared prefix.** It is the longest
  leading-whitespace prefix common to every non-blank content line and
  to the closing delimiter's line, compared as text, so a body indented
  with tabs and a delimiter indented with spaces share nothing and
  nothing is stripped.
- **A whitespace-only line becomes an empty line.**
- **Escapes decode after the strip.** `\n`, `\t`, `\\`, `\"`, `\0`,
  `\xNN`, and `\u{...}` mean what they mean in `"..."`, and a `\n`
  written in the body is a newline escape rather than a line break.
  Because the strip runs first, an escape can never shift the measure.
- **`"""abc"""` on one line** takes its body verbatim, with no strip.
  It is the short way to write a string containing `"`.

## Values with and without a trailing newline

Both are expressible. The newline before a closing delimiter on its own
line is not part of the value, so a trailing newline is written as a
blank line:

```gossamer
let no_newline = """
one
"""
let with_newline = """
one

"""
```

## `gos fmt`

The formatter moves a triple-quoted body with the line that opens it.
Indent the statement one level further and the body, the relative
indentation inside it, and the closing delimiter all follow:

```gossamer
fn main() {
    let text = """
    body
    """
}
```

Relative indentation inside the block is preserved exactly, so the
literal's value never changes. The formatter's no-destruction gate
compares such a literal by the contents it decodes to rather than by the
text that spells it, so a re-indent that would alter the value is
refused rather than written.
