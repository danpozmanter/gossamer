# Toolchain reference

Every subcommand of `gos`. Auto-generated output coming with
Stream H polish — for now this page is hand-written and may lag
the implementation by a rev.

## Front-end

| Command | Purpose |
|---------|---------|
| `gos parse FILE` | Print the AST. |
| `gos check [--timings] FILE` | Parse + resolve + typecheck + exhaustiveness. With `--timings`, prints per-stage wall-clock times. Parse output is cached by source hash — re-invocations on an unchanged file reuse the parsed AST. Set `GOSSAMER_CACHE_TRACE=1` to log cache hits. |
| `gos run [--vm] FILE` | Execute via the tree-walker (or VM). |
| `gos build [--target TRIPLE] FILE` | Produce a native binary (ELF/Mach-O/PE) by lowering through MIR + Cranelift and linking the user's `.o` against `libgossamer_runtime.a`. Every legal program compiles; a build error means a compiler bug. |

## Formatting + linting + docs

| Command | Purpose |
|---------|---------|
| `gos fmt [--check] FILE` | Rewrite canonically. |
| `gos doc [--html OUT] FILE` | List items (plain-text) or write an HTML page. |
| `gos lint [--deny-warnings] [--explain ID] [--fix] PATH` | Run the lint suite (50 lints). `--fix` writes auto-applicable suggestions back to disk; `--explain ID` prints long-form rationale. |
| `gos explain CODE` | Long-form rationale for a diagnostic code. |

## Testing + benchmarking

| Command | Purpose |
|---------|---------|
| `gos test PATH` | Run `#[test]` functions **and** doc-tests extracted from `` ``` ``-fenced code inside `//` doc comments. `` ```text `` and other language tags are skipped. Accepts a file or a directory. |
| `gos bench [--iterations N] FILE` | Time `#[bench]` functions. |

## Watch

| Command | Purpose |
|---------|---------|
| `gos watch [--command CMD] PATH` | Re-run the inner command on file change. |

## Housekeeping

| Command | Purpose |
|---------|---------|
| `gos clean [--vendor] [--dry-run]` | Remove toolchain-produced artefacts. By default wipes the frontend parse cache. `--vendor` also deletes `./vendor/`. `--dry-run` reports what would be removed without touching anything. |

## Package manager

| Command | Purpose |
|---------|---------|
| `gos new ID [--path DIR] [--template bin\|lib\|workspace]` | Scaffold a project. |
| `gos init ID` | Create `project.toml` in the CWD. |
| `gos add SPEC` | Add a dependency (`name` or `name@version`). |
| `gos remove ID` | Drop a dependency. |
| `gos tidy` | Canonicalise the manifest. |
| `gos fetch` | Populate the local cache. |
| `gos vendor` | Copy fetched deps into `./vendor/`. |

## REPL

`gos` with no arguments — or `gos repl` — drops into an
interactive session. The first-slice supports:

- Numbered `In [N]:` / `Out[N]:` prompts, coloured green / red
  when stdout is a TTY (ipython-style).
- Declarations persisting across inputs (`fn` / `struct` / `enum`
  / `use` / `const` / `type`).
- `let` bindings persisting across inputs; every subsequent
  expression sees previously-bound locals in scope. `%bindings`
  lists the active set.
- Meta-commands `%quit`, `%history`, `%bindings`, `%reset`,
  `%help`.
- Ctrl-D exits cleanly.

Stream K grows this to IPython parity (syntax highlighting, tab
completion, persistent history file, `%time` / `%timeit` /
`%load` / `%save` / `%edit` / `%debug`).

## Editor integration

| Command | Purpose |
|---------|---------|
| `gos lsp` | Start a language-server-protocol adapter on stdio. |

`gos lsp` is intended for editors, not humans. Shipped
capabilities:

- `textDocument/publishDiagnostics` on `didOpen` / `didChange` —
  every open document runs through parse + resolve + typecheck and
  diagnostics are published inline.
- `textDocument/hover` — renders a small markdown card with the
  identifier under the cursor and its interned type when the
  type checker can resolve it.
- `textDocument/definition` — jumps to the declaring item for
  identifiers that resolve to a top-level `fn` / `struct` / `enum`
  / `trait` / `type` / `const` / `static` / `mod`.
- `textDocument/completion` — completion provider for top-level
  items and keywords in scope.
- `textDocument/references` — every whole-word occurrence of the
  symbol under the cursor, in the same document. Matched
  syntactically; shadowed locals are reported alongside the real
  references until the semantic use-to-def map lands.
- `textDocument/prepareRename` + `textDocument/rename` — returns
  a `WorkspaceEdit` that renames every occurrence of the symbol
  in the file. Rejects non-identifier `newName` inputs.

Editors should launch `gos lsp` over stdio and speak LSP 3.16 with
`textDocumentSync=Full` (incremental edits land in a follow-up).

## Smoke-test

```sh
python3 - <<'PY'
import json, subprocess
p = subprocess.Popen(["gos", "lsp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"processId": None, "capabilities": {}}}).encode()
p.stdin.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body); p.stdin.flush()
print(p.stdout.readline(), p.stdout.readline())
PY
```
