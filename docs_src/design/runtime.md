# Runtime internals

This page is a map — not a specification — of what happens between
`gos run` and `main` returning. Each section links to the crate that
owns the stage so a new contributor can find the real source.

## Stages

```
source.gos
   │
   ▼
┌──────────────┐  gossamer-lex        tokens + source map
│  Lexing      │
└──────────────┘
   │
   ▼
┌──────────────┐  gossamer-parse      AST (items + uses)
│  Parsing     │  gossamer-ast        + diagnostics
└──────────────┘
   │
   ▼
┌──────────────┐  gossamer-resolve    name resolution, imports
│  Resolution  │                      path → DefId mapping
└──────────────┘
   │
   ▼
┌──────────────┐  gossamer-types      type inference, trait solve,
│  Type check  │                      exhaustiveness
└──────────────┘
   │
   ▼
┌──────────────┐  gossamer-hir        lowered program tree
│  HIR lower   │                      (match-desugars, for → loop)
└──────────────┘
   │
   ▼
┌──────────────┐  gossamer-interp     tree-walker VM; the bytecode
│  Evaluation  │                      and Cranelift backends live in
└──────────────┘                      gossamer-mir / -codegen-*.
```

## Evaluator

The tree-walker in `gossamer-interp` is the default engine. It:

1. Accepts an `HirProgram`.
2. Installs every top-level function and inherent-impl method under
   both the unqualified (`foo`) and type-qualified (`Type::foo`)
   names in a `HashMap<String, Value>`.
3. Registers builtin callables for stdlib functions (`os::args`,
   `time::sleep`, `json::parse`, …) and variant constructors for
   every user enum.
4. Walks HIR expressions directly, keeping local bindings in an
   `Env` stack.

Struct values are `Rc<Vec<(Ident, Value)>>`. Field assignment runs
through a copy-on-write helper that allocates a fresh `Rc` so alias
bindings never observe each other's mutations.

## Garbage collector

`gossamer-gc` is the off-line design of the real concurrent GC — a
tri-colour mark-sweep collector with write-barriers for generations
and weak references. The tree-walker currently piggy-backs on Rust's
`Rc` / `Arc` reference counting; the concurrent GC comes online once
the Arc-based interpreter lands.

## Scheduler

`gossamer-sched` holds the scheduler skeleton. Today `go expr` in
the tree-walker is inlined (the body runs on the calling stack);
real parallelism arrives with the Arc-based `Interpreter: Send`
transition that lets the scheduler own a real
worker pool.

## HTTP server

`gossamer-std::http::server::bind_and_run` runs an accept-loop on
the main thread and spawns one OS thread per accepted connection.
Each worker parses the request, sends the parsed form over an
`mpsc::channel` back to the interpreter thread, and awaits a
response on a return channel.

Graceful shutdown is driven by:

- `GOSSAMER_HTTP_MAX_REQUESTS=N` — env var, stop after N requests.
- `gossamer_interp::set_http_max_requests(N)` — safe-Rust test hook.
- `config.shutdown: Arc<AtomicBool>` — for in-process callers.

## Goroutines

Today `go expr` evaluates `expr` inline — the body runs to
completion on the caller's stack before the `go` expression
finishes. A real scheduler-backed implementation depends on two
prior pieces:

1. `Value: Send` (the Arc-based interpreter).
2. A multi-worker dispatch in `gossamer-sched`.

Both land together under the risks backlog.

## Panic recovery

`panic(msg)` in user code returns `RuntimeError::Panic(msg)` from
the evaluator. The native HTTP server catches that per-request,
logs it, and returns a 500. A panic inside a `go expr` body crashes
the process today because the tree-walker runs the body inline —
proper goroutine panic isolation lands with the scheduler (§1.6).

## Where each stage is tested

| Stage | Test location |
|-------|---------------|
| Lexing | `gossamer-lex/tests/` |
| Parsing | `gossamer-parse/tests/` |
| Resolution | `gossamer-resolve/tests/smoke.rs` |
| Type check | `gossamer-types/tests/typeck.rs`, `tests/exhaustiveness.rs` |
| HIR lower | `gossamer-hir/tests/lower.rs` |
| Interpreter | `gossamer-interp/tests/{eval,run_pass,vm,http_end_to_end}.rs` |
| Stdlib | `gossamer-std/src/*` (`#[cfg(test)]` modules) |
| Driver | `gossamer-driver/tests/` |
| CLI | `gossamer-cli/tests/cli.rs` |
