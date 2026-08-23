# Sandboxing

Three things share one policy model: a compile-time capability policy,
a sandbox around the build, and a standalone command sandbox. A policy
means the same thing whichever one compiled it.

## Compile-time evaluation: `--comptime-io`

A `comptime { ... }` region and every `comptime fn` call evaluate on the
bytecode VM while the program is being compiled, with the privileges of
whoever started the compile - including on `gos check`, which an editor,
the language server, the MCP server, and CI run continuously on code
they did not write.

`--comptime-io` bounds what that evaluation may reach:

| Level | Compile-time capabilities |
|---|---|
| `none` | No I/O at all. A `comptime` region is pure computation over its inputs. |
| `confined` (default) | Reads under the source tree. Writes, process spawn, network, environment mutation, and reads that leave the tree are denied. |
| `full` | Everything the compiling user can do. Never a default. |

```
gos check app.gos                        # confined
gos check --comptime-io=none app.gos     # no compile-time I/O at all
gos build --comptime-io=full app.gos     # the explicit escape
```

A read is compared against the canonical path, so a symlink pointing out
of the source tree is denied at its target. A denied call reports
`GX0010` naming the builtin, the capability class, and the option that
would permit it; `gos explain GX0010` has the long form.

`codegen!` is unaffected at every level: splicing a `comptime fn`'s
`String` is pure computation, so the metaprogramming story survives
`none` intact.

A project pins its posture once:

```toml
[project]
comptime-io = "none"
```

The toolchain takes **the more restrictive of the manifest and the
command line**. That is not a style choice: the manifest is written by
whoever wrote the project, which is exactly the party the policy defends
against, so a hostile `comptime-io = "full"` in a fetched dependency
resolves against the `confined` default and loses. Tightening from the
manifest is free, which is what lets a project adopt `none`
permanently.

## The build: `gos build --sandbox`

`[rust-bindings]` compilation runs Cargo, which runs dependency
`build.rs` files, procedural macros, and a linker. `--sandbox` puts all
of it inside an OS-native policy.

```
gos build --sandbox                # same as --sandbox=standard
gos build --sandbox=strict
gos check --sandbox                # covers check, doc, repl, run, test too
gos build --sandbox-explain        # print the policy, build nothing
```

The flag attaches where Cargo is invoked, not at the `build`
subcommand, so it covers every subcommand that compiles bindings.
Attaching it to `build` alone would leave five doors open, including
`check`.

**The run is split.** Downloading a dependency is inert; executing what
was downloaded is not. So `cargo fetch` runs with the network and
without running any dependency code, and then `cargo build --offline`
runs with the network denied and `build.rs`, proc macros, and the linker
inside the policy.

| Flag | Effect |
|---|---|
| `--sandbox[=LEVEL]` | `none` (default this release), `basic`, `standard`, `strict` |
| `--sandbox-network` | Let the build phase reach the network too |
| `--sandbox-rw PATH` | Add a read-write grant. Repeatable |
| `--sandbox-ro PATH` | Add a read-only grant. Repeatable |
| `--sandbox-explain` | Print the compiled policy and the mechanisms, build nothing |

`project.sandbox = "standard"` raises a project's floor permanently. A
grant never lifts a denial, so `--sandbox-ro ~/.ssh` is refused as a
policy error rather than honored.

`--sandbox` does not sandbox your own program under `gos run`. For
that, run it under a sandbox of your own: `std::sandbox` builds one, and
`build-with-restrictions` runs any command under one.

## Any command

The same policy model is also a standalone application,
`build-with-restrictions` (`bwr`), which runs any command - `cargo
build`, `npm ci`, `./gradlew build`, an untrusted binary - under the
sandbox this crate provides. It ships separately from the Gossamer
toolchain and carries build-system profiles of its own: where a tool
keeps its caches is the wrapper's knowledge, not the language's.

From Gossamer, `std::sandbox` reaches the same library directly; see
below.

## Levels

A level name means the same **guarantee** on every operating system. A
host that cannot meet one reports it unavailable and names the blocking
primitive; it never offers a weaker thing under the same name.

| Level | Guarantee | Linux | macOS | Windows |
|---|---|---|---|---|
| `none` | No sandbox | yes | yes | yes |
| `basic` | Environment allowlist, private temp, descriptor and handle hygiene, tree cleanup | yes | yes | yes |
| `standard` | OS-enforced filesystem policy and network denial, inherited by every descendant | Landlock + netns | Seatbelt profile | restricted token + job object |
| `strict` | `standard` plus process-table isolation and a reduced kernel surface | namespaces + seccomp | **unavailable** | AppContainer |

macOS reporting `strict` unavailable is a feature of the model, not a
gap papered over: the platform has no process-namespace equivalent, and
saying so is what makes the other levels believable.

`sandbox::capabilities_json()` reports what this host actually honors,
including which Landlock ABI the kernel offers and which sysctl blocks
`strict`. `gos build --sandbox-explain` prints the same for the build
policy.

## From Gossamer: `std::sandbox`

The same policy model, reachable from a program:

```gossamer
use std::sandbox

fn main() {
    let policy = sandbox::Policy::new()
        .read_write(&".")
        .read_only(&"/usr")
        .network_mode(&"none")
        .env_allow(&"PATH")
        .level(&"standard")

    // The capability report is a value, so a program branches on what
    // the host honors instead of assuming one operating system.
    if sandbox::max_level() == "strict" {
        println!("{}", sandbox::notes().join(&"\n"))
    }

    match sandbox::run(&policy, &#["cargo", "build"]) {
        Ok(out) => println!("{} {}", out.code, out.stdout)
        Err(e) => eprintln!("{}", e)
    }
}
```

`sandbox::Policy::command_default(&cwd)` is the shipped policy as a
constructor, so a program reproduces it without reassembling a dozen
grants and getting one wrong. `sandbox::run` blocks for the length of
the child but does so off the scheduler, so one sandboxed build does not
hold a worker.

A policy says what a command may reach. Bounding how long it runs or how
much memory it takes is the caller's own business: those were policy
settings once, and two of the three backends could only partly apply
them, which is a guarantee in name only.

## What is not claimed

Not a defense against a kernel local-privilege-escalation exploit. Not
equivalent to a VM or a hypervisor boundary. Not a defense against a
hostile process already running as another user. Not a side-channel
boundary. On macOS, not a supported-API boundary at all: the platform
offers no supported public API for sandboxing an arbitrary child.

`SECURITY.md` carries these limits, and every escape that cannot be
closed is documented with the mechanism named.
