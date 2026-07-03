# `lang::macros`

Status: shipped

Built-in macros only - no user-defined macros: the format family (print/println/eprint/eprintln/format/panic), the desugar macros (matches!/todo!/unimplemented!/unreachable!/dbg!), and the build-time regex!/sql!/codegen!.
<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

The macro set is fixed. Every `name!(...)` outside this list is a parse
error (`GP0001`) - there are no user-defined macros.

## Format macros

`format!`, `println!`, `print!`, `eprintln!`, `eprint!`, and `panic!`
take a literal template plus arguments. Placeholders are positional `{}`,
named `{ident}` (a binding in scope), and `{:spec}` / `{name:spec}` with
Rust's width / fill / align / radix / precision grammar:

```gossamer
let name = "jane"
println!("hello, {name}")          // named capture
println!("[{:>8.2}]", 3.14159)     // [    3.14]
let msg = format!("{} / {}", 1, 2) // returns a String
```

A named capture also walks a field path: struct fields (`{a.balance}`),
tuple indices (`{t.0}`), nesting (`{o.inner.hits}`), and specs on the
path (`{a.balance:>8}`, `{f.0:.2}`) all resolve against bindings in
scope:

```gossamer
struct Account { owner: String, balance: i64 }
let a = Account { owner: "jane", balance: 1200 }
println!("{a.owner}: {a.balance:>8}")
```

Any other expression in a placeholder (`{age + 1}`, `{v[i]}`) is a parse
error (`GP0021`) - bind it first or pass it positionally.

## Desugar macros

These expand at parse time into ordinary constructs, so they lower
uniformly on every tier:

- `matches!(expr, pat)` - boolean: does `expr` match the pattern.
- `todo!` / `unimplemented!` / `unreachable!` - `panic!` with a fixed (or
  supplied) message.
- `dbg!(expr)` - prints `expr` with `{:?}` to stderr and yields its value.

```gossamer
if matches!(n, 1..=9) { /* single digit */ }
let x = dbg!(compute())   // logs the value, returns it
```

## Build-time macros

`regex!("…")` and `sql!("…")` validate their literal argument at compile
time and fold to the validated string; `codegen!(...)` splices a `comptime
fn`'s `String` result back as source. See [comptime](comptime.md).
