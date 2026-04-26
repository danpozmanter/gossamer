# Gossamer diagnostics — style guide

Every diagnostic the compiler emits goes through
`gossamer-diagnostics`. A diagnostic has exactly four things the user
sees:

1. A **severity** — `error`, `warning`, `note`, or `help`.
2. A **code** — a stable four-character prefix plus a four-digit
   number (`GP0001`, `GR0003`, `GT0004`, …).
3. A one-line **title** that fits in ~72 columns.
4. A list of **labels**, **notes**, **helps**, and optional
   **suggestions**.

The CLI renders them as rustc/elm-style boxes. Tests assert against
the stable `render_plain` form or against the code.

## Severity

| Severity | When to use |
|----------|-------------|
| `error` | Compilation cannot proceed. Parser, resolver, type checker, exhaustiveness, and lint-as-error. |
| `warning` | Code compiles but is suspicious. Default for lints that catch real bugs. |
| `note` | Extra context attached to another diagnostic. |
| `help` | Actionable advice attached to another diagnostic. |

Never emit a bare `error` / `warning`. Always carry a code.

## Codes

Allocate from the phase-scoped namespace. New codes go in the next
unused slot for that phase.

- `GP` — parser and lexer.
- `GR` — name resolution.
- `GT` — type checker.
- `GM` — match exhaustiveness.
- `GL` — lints.
- `GK` — package manager.

Once published, **never reuse** a code. If a diagnostic is removed,
its code is retired — `gos explain <CODE>` will say "retired in
version X, see <CODE>".

## Titles

- Present tense, declarative. Yes: "unexpected token `{`". No:
  "error: a token was unexpected here".
- Lowercase (the severity in front capitalises for you).
- Under 72 characters. If you cannot say it in 72, split into a
  title + note.
- Do not mention the error code in the title — the renderer prints
  both.
- Do not mention the file name — the renderer prints the location.

## Labels

Every error has one **primary label** pointing at the smallest span
that localises the problem. Secondary labels fill in supporting
context (the prior declaration, the matching brace, the earlier
binding).

- Primary label messages are one phrase, lowercase: `"unexpected
  `fn`"`, `"expected `;`"`, `"type declared here"`.
- Secondary label messages match the same style; they read
  naturally when the reader hops from one span to the other.

## Notes and helps

`note:` lines explain *why* the error fires. `help:` lines suggest
a fix.

- At most one of each per diagnostic. Prefer one complete sentence
  over two half-sentences.
- Lead with the imperative. Yes: "add `mut` before `x`". No: "you
  could add `mut`".
- No references to internal compiler phases. The user does not know
  what "HIR lowering" is.

## Suggestions

Machine-applicable fix-its carry three fields: a `Location`, a short
message, and a replacement string. `gos lint --fix` (Stream C) and
editors consume them.

- Apply them through the `gos fmt --apply-fixes` path. Never apply
  silently during `gos check`.
- Suggestion messages start with a verb: `"rename to `new_name`"`,
  `"remove this clone"`, `"wrap in `Some(...)`"`.
- The replacement string replaces exactly `Location.span` — no more,
  no less. If more context is needed, widen the location.

## Did-you-mean

When a name lookup fails, the resolver attempts a Levenshtein match
against the names in scope (edit distance ≤ 2). If a match exists,
it is attached as a `help:`. `gossamer_diagnostics::suggest` is the
shared helper.

## Anti-patterns

- No stack traces. The compiler is not a runtime.
- No "internal error" shown to the user without an issue-report
  link. Prefer an `assert` over a vague panic.
- No diagnostic without a location — even "could not open file"
  should carry a synthetic zero-length span at offset 0.
- No chained `.context(...)` via `anyhow` at the user-facing layer.
  `anyhow` is for internal wiring; user-facing errors go through
  `Diagnostic`.

## Acceptance tests

Every error code has a fixture under
`crates/gossamer-diagnostics/tests/fixtures/<code>.gos` with a
`// ERROR:` marker comment. The test harness parses + checks + lints
the fixture and asserts the right code fires. New codes without a
fixture fail CI.

## Colours

The default renderer emits ANSI when `RenderOptions::colour` is set,
and plain text otherwise. Tests always use the plain form. The CLI
enables colour automatically when stderr is a TTY and honours
`NO_COLOR` / `CLICOLOR_FORCE`.
