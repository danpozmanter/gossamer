# Running Gossamer

Once `gos` is on your `PATH`, every subcommand takes either a
`.gos` source file, a project directory containing
`project.toml`, or no argument at all (drops into the REPL).

## Cheat-sheet

| Command | What it does |
|---------|--------------|
| `gos new example.com/app --path ./app` | Scaffold a project |
| `gos init example.com/app` | Scaffold just `project.toml` in the CWD |
| `gos run src/main.gos` | Tree-walk interpreter |
| `gos run --vm src/main.gos` | Register-based bytecode VM |
| `gos check src/main.gos` | Type-check + exhaustiveness |
| `gos build src/main.gos` | Native build — links user code against the `gossamer-runtime` staticlib and lets `cc` produce an ELF/Mach-O/PE. Every legal program compiles; a codegen bail is a compiler bug. |
| `gos build --target aarch64-apple-darwin src/main.gos` | Cross-compile. Reserved for a future milestone — currently rejected because the native path only targets the host ISA. |
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

## Environment variables

- `GOSSAMER_HTTP_MAX_REQUESTS=N` — ask the HTTP server to exit
  after `N` requests. Used by CI tests; leave unset for normal
  operation. A visible warning prints when the env var is
  honoured.
- `NO_COLOR` / `CLICOLOR_FORCE` — standard colour toggles.
- `EDITOR` — used by the REPL's `%edit` meta-command (Stream K
  follow-up).

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Subcommand-reported failure |
| 2 | Clap argument parsing failure |
| 101 | Panic from the compiler (file a bug) |
