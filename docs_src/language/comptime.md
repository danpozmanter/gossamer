# `lang::comptime`

Status: shipped

Zig-style compile-time evaluation: `comptime { ... }` blocks, `comptime fn` calls, and `comptime` parameters run on the bytecode VM during compilation and fold to a literal, so every tier compiles the identical constant. `typeInfo::<T>()` reflects a type's fields, a `for (name, ty) in typeInfo::<T>()` loop unrolls into native per-field code, and `codegen!(...)` splices a `comptime fn`'s `String` back as source. Includes the `regex!` / `sql!` build-time validation macros.
<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

Zig-style compile-time evaluation. `comptime` runs ordinary Gossamer on
the bytecode VM *during compilation* and folds the result into the
program as a literal, so the bytecode VM, the Cranelift JIT, and the
LLVM AOT backend all compile the identical constant - comptime never
reaches a backend. There is no macro grammar, no hygiene model, and no
token-tree DSL: the metaprogramming you learn is just the language.

## Comptime blocks and functions

A `comptime { ... }` block evaluates its body at compile time; a
`comptime fn`'s calls are folded at every call site.

```gossamer
comptime fn fib(n: i64) -> i64 {
    if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
}

// The recursion runs at compile time; the binary embeds 6765.
const TABLE: i64 = comptime { fib(20) }

fn main() {
    let triangular = comptime {
        let mut acc = 0
        for i in 1..=100 { acc += i }
        acc
    }
    println!("{}", triangular)   // 5050
    println!("{}", fib(10))      // 55, computed at build time
}
```

A comptime region may use the full language - `let`, loops, `if` /
`match`, calls, string building - as long as every value it reads is
compile-time-known (literals, consts, `comptime fn` results).
Referencing a runtime binding is a compile error. A region must
evaluate to a scalar (`i64` / `u64` / `bool` / `char` / float) or a
`String`.

This also lets a `const` perform work the constant folder cannot do on
its own: `const T: i64 = comptime { fib(20) }` compiles natively, where
the bare `const T: i64 = fib(20)` form (a non-inlinable call in a const
initializer) does not.

## Comptime parameters

A function parameter declared `comptime` has its argument evaluated at
compile time and replaced with the result literal, while the function
itself runs normally:

```gossamer
fn scale(comptime factor: i64, x: i64) -> i64 { factor * x }

fn main() {
    let x = read_runtime_value()
    // `BASE * 2 + 5` is folded to a constant at the call site.
    println!("{}", scale(BASE * 2 + 5, x))
}
```

Passing a non-compile-time-known argument to a `comptime` parameter is
a compile error.

## Reflection - `typeInfo`

`typeInfo::<T>()` reflects a struct's fields at compile time, returning
`[(String, String)]` of each field's `(name, type)`. A `comptime fn`
consumes that to generate per-type code:

```gossamer
struct User { id: i64, name: String, active: bool }

comptime fn create_table(table: String, fields: [(String, String)]) -> String {
    let mut cols = ""
    let mut first = true
    for (name, ty) in fields {
        if !first { cols += ", " }
        first = false
        let sql_ty = match ty {
            "i64" => "INTEGER",
            "String" => "TEXT",
            "bool" => "BOOLEAN",
            _ => "BLOB",
        }
        cols += name + " " + sql_ty
    }
    "CREATE TABLE " + &table + " (" + &cols + ")"
}

// Evaluated at build time; the binary embeds the finished DDL string.
const USER_DDL: String = comptime { create_table("users", typeInfo::<User>()) }
// CREATE TABLE users (id INTEGER, name TEXT, active BOOLEAN)
```

The reflection is resolved at compile time and nothing about it survives
into the running binary - the emitted code is exactly the hand-written
string.

## Code generation from reflection

Folding to a constant *value* is the floor; reflection also generates
native *code*. Both shapes resolve entirely at compile time - the emitted
body is ordinary native field code, identical on every tier with no
runtime reflection.

A plain `for (name, ty) in typeInfo::<T>()` loop is unrolled once per
field in the single compile (no fold pass). `name` / `ty` are comptime,
`field_of(v, name)` projects the concrete field, and a `match` over the
comptime `ty` folds to the taken arm. Written once as a generic
`fn rec<T>` and specialised per turbofish call site, one reflection-driven
serializer covers every struct - the `autoderive` shape, in user space:

```gossamer
struct User { id: i64, name: String, active: bool }

fn record<T>(v: T) -> String {
    let mut out = ""
    for (name, ty) in typeInfo::<T>() {
        let label = match ty {
            "String" => "str",
            "bool" => "bool",
            _ => "int",
        }
        out += name + ":" + label + "=" + format!("{}", field_of(v, name)) + ";"
    }
    out
}

// id:int=7;name:str=jane;active:bool=true;
println!("{}", record::<User>(User { id: 7, name: "jane", active: true }))
```

A concrete-type loop (`for (name, ty) in typeInfo::<Point>()`) needs no
turbofish and is unrolled directly.

`codegen!(...)` - splices a `comptime fn`'s `String` result back as raw
source, for generation beyond the field-loop shape. The spliced
expression is type-checked in place, so it must produce the type the call
site expects:

```gossamer
comptime fn gen_show(fields: [(String, String)], v: String) -> String {
    let mut out = "\"\""
    for (name, _) in fields {
        out += " + \"" + name + "=\" + format!(\"{}\", " + v + "." + name + ") + \" \""
    }
    out
}

fn show(p: Point) -> String {
    codegen!(gen_show(typeInfo::<Point>(), "p"))   // emits: "" + "x=" + ... + " "
}
```

## Compile-time validation - `regex!` / `sql!`

`regex!("…")` and `sql!("…")` validate their argument at build time and
fold to the validated string. A malformed pattern or statement fails the
build with a diagnostic rather than reaching runtime - the project's
"if it compiles, it works" goal:

```gossamer
let pattern = regex!("^\\d{4}-\\d{2}-\\d{2}$")   // compiled + checked at build time
let query   = sql!("SELECT id, name FROM users WHERE id = 1")
```

`regex!("(unclosed")` fails the build with `unclosed group`; an
unbalanced `sql!` fails with a parenthesis error. These are the only
compile-time-validation macros; every other `name!(...)` outside the six
format macros remains a parse error (`GP0001`).

## How it works

After parsing, resolving, and typechecking, the compiler loads the
program onto the bytecode VM and evaluates every comptime region - a
`comptime { ... }` block, a `comptime fn` call, or a `comptime`
parameter's argument. Each result is spliced back into the source as a
literal, and the program is recompiled normally. A `for` over
`typeInfo::<T>()` skips the fold pass entirely: it is unrolled per field
in the single compile. Either way the substitution happens before the
tiers diverge, so all three tiers compile the same code: tier parity is
automatic.

## Scope

The shipped surface is compile-time *evaluation*, *reflection over
named structs*, *comptime parameters*, *build-time validation*, and
*code generation* (a `for` over `typeInfo::<T>()` and `codegen!`).
Comptime regions fold to scalar or string results. You can now write the
reflection-driven `autoderive` shape yourself, but the built-in
`to_json` / `from_json` serializers remain built in (not yet re-expressed
as comptime library code); `comptime { to_json(value) }` runs them at
build time. See `SPEC.md` section 14.
