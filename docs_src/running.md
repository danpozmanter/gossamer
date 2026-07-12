# Running Gossamer

Once `gos` is on your `PATH`, every subcommand takes either a
`.gos` source file, a project directory containing
`project.toml`, or no argument at all (drops into the REPL).

## Cheat-sheet

| Command | What it does |
|---------|--------------|
| `gos new example.com/app --path ./app` | Scaffold a project |
| `gos init example.com/app` | Scaffold just `project.toml` in the CWD |
| `gos run src/main.gos` | Register-based bytecode VM with in-process Cranelift JIT |
| `gos run --no-jit src/main.gos` | Same VM, pure bytecode dispatch (JIT off) |
| `gos check src/main.gos` | Type-check + exhaustiveness |
| `gos build src/main.gos` | Native build via LLVM AOT - lowers through MIR + LLVM (`llc -O0`), then links the user's object against the `gossamer-runtime` staticlib into an ELF/Mach-O/PE. |
| `gos build --release src/main.gos` | Optimised native build - full LLVM `opt -O3 \| llc -O3` pipeline, static musl on Linux. |
| `gos build --target aarch64-unknown-linux-musl src/main.gos` | Cross-compile to a Tier 2 Linux-musl target. CI executes the resulting AOT output under QEMU and compares it with the pure bytecode VM. Other registered triples are not necessarily supported deployment targets. |
| `gos fmt src/main.gos` | Rewrite canonically; `--check` refuses to edit |
| `gos doc src/main.gos` | List items + docstrings |
| `gos test src/main.gos` | Discover and run `#[test]` functions |
| `gos bench src/main.gos` | Time `#[bench]` functions |
| `gos lint .` | Run the lint suite over a directory |
| `gos lint --deny-warnings .` | Fail CI on any warning |
| `gos lint --explain unused_variable` | Long-form rationale for a lint |
| `gos explain GT0001` | Long-form rationale for a diagnostic code |
| `gos watch --command check .` | Re-run `gos check` on every change |
| `gos add example.org/lib@1.2.3` | Add a dependency to `project.toml` |
| `gos remove example.org/lib` | Drop a dependency |
| `gos tidy` | Re-canonicalise the manifest |
| `gos fetch` / `gos vendor` | Populate the package cache / vendor tree |
| `gos` (no args) | Interactive REPL |
| `gos repl` then `%help strings::trim` | Show stdlib or language help from the manifest |
| `gos repl` then `%ls strings` | List stdlib modules or module contents |

## Entry file

`gos run file.gos` and `gos build file.gos` accept a file with no
`fn main`: bare statements at the top of the entry file become the
body of an implicit `fn main()`. So a one-line `println!("Hello
World")` file runs as-is. See
[Top-level statements](language/top_level_statements.md). A project's
entry file is `src/main.gos` by convention, or whatever
`[project] entry` names in `project.toml`.

## Environment variables

- `GOSSAMER_HTTP_MAX_REQUESTS=N` - ask the HTTP server to exit
  after `N` requests. Used by CI tests; leave unset for normal
  operation. A visible warning prints when the env var is
  honoured.
- `NO_COLOR` / `CLICOLOR_FORCE` - standard colour toggles.
- `EDITOR` - used by the REPL's `%edit` meta-command (Stream K
  follow-up).

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Subcommand-reported failure |
| 2 | Clap argument parsing failure |
| 101 | Panic from the compiler (file a bug) |
