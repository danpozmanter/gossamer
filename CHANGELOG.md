# Changelog

## 0.49.0 - Delimited lists, iterator contracts, explicit dependency imports

- A `let` that reuses the name of a consumed iterator or range introduces a
  fresh, unconsumed binding. Consumption tracked the name rather than the
  binding, so shadowing a consumed name reported GT0042 on the new binding for
  the rest of the scope, and a REPL session could not redefine the name at all
  once any pipeline had taken it.
- `%bindings` lists a consumed iterator with its type and value. Observing a
  binding to report it is not a traversal; a written read of one stays an
  error.
- A newline separates the elements of every delimited list, not a subset.
  Tuples and tuple types, `Vec` / array / `Map` / `Set` literals, tuple, slice,
  and struct patterns, generic parameters and arguments, and `use` lists join
  the forms that already allowed it. `gos fmt` removes multiline commas from
  parenthesised and map-literal lists, so a formatted multi-line tuple or map
  no longer parses as anything at all. A newline separates only where a comma
  could, leaving `(\n a + b \n)` the parenthesised expression it reads as.
- `%info` states the type an iterator call actually has. `Range::map` and every
  adapter on a `Range` receiver reported the materialised `Vec` return of the
  eager catalog row, and `take_while` / `skip_while` reported it on `Iterator`
  too, while all of them evaluate to `Iterator<T>`. Type checking and the
  rendered signature now read one lazy-versus-terminal classification.
- Reaching a dependency's items requires the import that names the package.
  `intcode::run(..)` resolved with no `use "example.com/intcode"` in the file,
  leaving the path's provenance unstated; the bare path reports GR0016 with the
  import to add. Modules of the current project keep the weaker rule.
- Name a dependency's type through a renaming alias. `use "example.com/dep" as
  d` reached `d::Item` in expressions but not in a type annotation, where the
  alias went unresolved.
- Resolve a project's `[rust-bindings]` from the entry a command names.
  `gos run path/src/main.gos` took its manifest from the working directory
  while `gos build` took it from the entry, so the same file compiled natively
  and was rejected by the front end on the interpreter.
- Compare a fixture whose output order the scheduler chooses on exit status
  alone. The tier-parity walk inferred nondeterminism by re-running the
  reference once, so two runs that happened to agree made a concurrent
  fixture's equally valid interleaving read as a divergence.
- Pass the address of a primitive receiver to a `&self` or `&mut self` method
  the program implements for it. The call site handed over the value, so the
  body dereferenced the value itself: `i64`, `bool`, and `char` receivers
  faulted natively, `f64` returned a wrong number, and `&mut self` never wrote
  back. Receivers reached from a local, an element, a loop binding, and a type
  parameter that resolved to a primitive all carry an address now.
- Read an element of a generic container as the element type the instantiation
  chose. A `Vec<T>` element was read as a single scalar slot whatever `T`
  became, so a struct element reached a trait method as its first eight bytes.
- Write a mutation made through `m.or_insert(k, default)` back into the map.
  The interpreter handed back a copy, so `m.or_insert(k, #[]).push(v)` left the
  stored value empty while the compiled tiers mutated it in place.
- Drop an identity update (`x = x`) during MIR canonicalisation. It reached the
  JIT as a statement whose destination was also its source, and any loop
  containing one - including every `match` or `if` with an arm that folds to
  one - exhausted the stack before the program ran.
- Reject `iter::flatten` over elements that are not sequences, and `iter::unzip`
  over elements that are not pairs. Both were accepted with an invented element
  type: `flatten` over a range segfaulted natively, and `unzip` returned
  different wrong values on each tier.
- Type `iter::filter_map`, `iter::flat_map`, and `iter::scan` as the lazy
  adapters they are, and accept an iterator argument for them. Their runtime
  went lazy under the 2027 edition while the checker still called the result a
  `Vec`, so the interpreter reported an empty result where the native build
  reported the real one.
- Correct the declared argument order of `iter::fold` and `iter::scan`: both
  take the accumulator first and the data last. The documented order did not
  compile, and `scan`'s declared closure and result types described neither the
  runtime nor any call site.
- Type `iter::empty()` and `iter::unzip(..)` results instead of leaving them
  unresolved.

## 0.48.1 - Allocation speed, swap contract, float parity

- Restore allocation-bound speed. The heap-profile hook added in front of
  mimalloc was a real call on every allocation; it now inlines into
  `__rust_alloc` and costs a predicted branch while disarmed. Allocation-heavy
  programs were up to 17% slower.
- `xs.swap(i, j)` returns unit instead of `Result<(), errors::Error>`, so the
  bare call is a complete statement anywhere its value is discarded, including
  a loop body's tail. A negative index, or one at or past the end, is a bounds
  panic on every tier, matching `xs[i] = v`. The declared `Err` was never
  reached: three of the four execution paths silently left the receiver
  unchanged instead. The panic names both indices and the receiver's length
  whichever specialization ran; the flat-register path reported a bare
  `index out of bounds`.
- Read a one-field struct's field through an index into a format argument.
  The field name and the struct's field-name list shared a constant-pool slot
  whenever the struct had exactly one field, and `xs[i].f` panicked on the VM
  and the JIT.
- `a * b + c` and `c - a * b` on f64 round twice on the bytecode VM, matching
  the compiled tiers' separate multiply and add. The VM fused each into a
  single-rounded `fma`, so any loop accumulating a product into a running
  total - a velocity update, a dot product, a weighted sum - printed different
  digits under `gos run` than under `gos build`.
- Print a `[u8; N]` fixed array with `{:?}` from a `gos build` binary. The
  formatter read byte-packed storage at one element per 8 bytes and printed
  garbage integers; element reads were already correct.
- `pprof::heap_profile` returns sampled allocation stacks from a `gos build`
  binary instead of a bare header. The sampler climbs out of the global
  allocator along the frame-pointer chain, and the runtime shims it passes
  through kept no frame pointer, so every walk recorded nothing. Those shims
  now carry one; the interpreter and the JIT do not, where it costs double
  digits on tight loops.
- The sampler's stack walk checks each frame-pointer link against the running
  stack's bounds before reading it. A link that passed the plausibility
  checks but addressed unmapped memory faulted the process.
- `pprof::heap_profile` records allocation stacks on aarch64, on Windows, and
  from `gos run`. The recorder started its walk from the frame-pointer
  register of its calling frame, which is an ordinary general register in code
  compiled without frame pointers, so the walk began at unrelated data and
  recorded nothing outside a compiled x86_64 binary. It now establishes a
  frame record of its own, and Windows walks with the OS unwinder.
- `pprof::cpu_profile` records stacks on macOS and on aarch64 Linux. The
  signal handler read the interrupted program counter and frame pointer only
  from the x86_64 Linux register file, and sampled an empty stack everywhere
  else.

## 0.48.0 - Evidence-backed feature status, source migrations, parity walk

- Report what is known about a surface, not what was typed about it.
  `gos feature-status` derives each row's lifecycle from evidence: a
  surface no fixture exercises reads `unproven`, which is different from
  `experimental` - a judgment someone made. The distribution moved from
  107 experimental / 55 shipped / 0 stable to 79 experimental / 69
  unproven / 14 shipped. 30 generated doc pages changed with it.
- Stop deriving tier support from the lifecycle label. `item_evidence`
  mapped `Shipped` to "runs on the VM" and `Stable` to "runs everywhere",
  so the field named evidence restated the claim it existed to support.
  Tiers now come only from a fixture that ran.
- Generate the fixture ledger with `cargo xtask item-fixtures`, from the
  tier-parity SPECS list and each fixture's imports: 2 hand-written
  entries became 66 modules drawn from 510 registered fixtures.
- Make `gos feature-status --check` capable of failing. An item may not
  claim a tier with no fixture behind it, and a surface reported as
  settled must pass every tier. Its only evidence-requiring branch had
  never executed, because it applied to `Stable` items and there were
  none.
- Ship the tier-parity evidence with the release, compiled into the
  binary, so an installed `gos` with no repository behind it reports what
  the walk proved. A CI job regenerates it and fails on drift.
- Resolve named arguments and parameter defaults in the playground. It
  drives the front end itself rather than going through the driver, and
  never ran that pass, so a call omitting a defaulted parameter reached
  the checker with fewer arguments than the function declares and was
  reported as an arity error. The tour's `arguments` lesson could not run
  in the browser while the same program ran natively.
- Sample CPU and heap profiles. `pprof::cpu_profile(millis)` arms a
  `SIGPROF` timer at 100 Hz and walks the interrupted thread's
  frame-pointer chain into a fixed buffer, allocating nothing and taking
  no lock, because the handler can interrupt code holding any lock in the
  process; addresses become names when the profile is drained.
  `pprof::heap_profile(millis)` records one stack per 512 KiB allocated,
  from inside the global allocator, using the same allocation-free walk.
  Both are exposed on every tier and restore `/debug/pprof/profile` and
  `/heap` to the router. Sampling costs nothing until it is asked for;
  instrumenting every function instead measured 2.7x on call-heavy code.
- Write the tour and the examples without the `fn main` wrapper the entry
  file makes optional. Nine examples keep it: seven are nondeterministic
  or embed line numbers in their output, and two carry a statement-level
  `#[lint(allow(..))]`, which at top level parses as an item attribute.
- Emit frame pointers in generated code. It has to be an IR function
  attribute: `clang -x ir` ignores `-fno-omit-frame-pointer`, which only
  sets the attribute when clang is the one generating the IR. Measured at
  no cost on a call-heavy benchmark, and it is what lets a profiler walk
  out of an arbitrary instruction, where DWARF unwinding is not
  async-signal-safe.
- Add `gos test --fuzz`: coverage-guided fuzzing of `#[fuzz]` functions
  over `&[u8]`, using the counters `--coverage` already reports as the
  feedback signal. A crash is minimised by delta debugging and written
  into `testdata/fuzz/<target>/`, where plain `gos test` runs it from
  then on - so a finding arrives as a deterministic test that fails until
  it is fixed, not as a report. `--seed` makes a run reproducible.
- Add `gos audit`: security advisories matched against the resolved
  lockfile and filtered by reachability, so an advisory naming an item
  the project never references is counted rather than printed. `--all`
  lifts the filter, `--format json` emits the shared diagnostic schema,
  and both are reachable over MCP. A registry feed is verified against a
  key the project pins in `[trusted-publishers]`, never one the registry
  supplies; with no pinned key there is no remote feed rather than an
  unverified one. `gos publish` names any reachable advisory and
  publishes anyway - refusing would put the feed in the path of every
  release.
- Resolve the project root when `gos publish` is given a bare manifest
  name. `Path::new("project.toml").parent()` is `Some("")` rather than
  `None`, so the root became the empty path and packing failed with a
  bare "No such file or directory".
- Report one row per exported item with `gos feature-status --items`: 911
  rows instead of 165. A module is the wrong unit for "can I rely on this
  call" - `std::strings` is one row whether an item has been there from
  the start or landed last week. Each item inherits its module's tier
  record. `--status` now filters on the status the table reports rather
  than the one that was authored.
- Add a `style` lint group naming one canonical spelling per construct:
  `GL0053` for a data-last `iter::` call that has a method form, and
  `GL0054` for the i64-only `collections::queue` family, which duplicates
  a general container at a narrower element type. Only `GL0053` is
  auto-fixable; the container families differ in what an empty container
  means, so a mechanical rewrite would keep type-checking and change the
  program.
- Add `project.enforce-format`, which makes `gos test` fail on any source
  that disagrees with `gos fmt`. Opt-in, so a project decides once that
  canonical formatting is part of passing.
- Add `gos fix`: deterministic, idempotent source migrations the
  toolchain owns, separate from `gos lint --fix`, which acts on
  observations about the code you wrote. Every rewrite is re-checked
  before it is kept and idempotence is verified on each run. Seeded with
  `method_form_combinators`, which rewrites `iter::map(f, xs)` to the
  canonical `xs.map(f)`. Reachable over MCP.
- Document the compatibility policy (`docs_src/compatibility.md`): what a
  patch, a minor, and an edition may change, and the rule that a change
  requiring hand edits is a defect in the release rather than work for
  the reader.
- Finish a full parity walk in minutes rather than hours. A fixture the
  VM cannot run to completion is no longer charged to the other two
  tiers, and a live process consuming no CPU is recognised as parked
  instead of waiting out its budget. A server example went from 60s to
  4s; the `examples/` walk from tens of minutes to three.
- Decide the walk on what a program does, not on the paths it prints or
  the order its goroutines finish. A program printing `argv[0]` reports
  the source path under the VM and the executable's path when compiled;
  output that moves between runs of one tier cannot be compared across
  tiers. Both were recorded as divergences.
- Compare the VM against the debug native build. The VM checks integer
  overflow and an optimised build wraps - a profile-dependent difference
  the language defines on purpose - so comparing against the release
  build asserted a behaviour that must differ.
- Score a fixture written to be rejected as agreement. Every tier refuses
  to build it, which is the tiers agreeing, not diverging.
- Block the calling thread on a socket that is not ready, rather than
  polling it at 1 kHz. `fn main() { listener.accept() }` runs outside a
  goroutine, where there is no coroutine to park.

## 0.47.1 - Profiles, applied fixes, canonical sources, agent tooling

- Add `std::pprof`: `goroutine_profile`, `mutex_profile`, `block_profile`,
  `execution_trace(millis)`, and `route(path, query)` for the
  `/debug/pprof/...` shapes. The three text profiles render the format
  `go tool pprof` reads and the trace is Chrome trace JSON. The generators
  live in the runtime, so the VM, the JIT, and both native builds render
  from one implementation over one set of scheduler counters. CPU and heap
  profiles need a sampler and are absent rather than empty.
- Apply the rewrites a diagnostic carries with `gos check --fix`. The
  suggestions were span-anchored and machine-applicable but nothing
  consumed them. Diagnostic suggestions and lint fixes are applied in
  separate rounds - an unresolved name makes its intended binding look
  unused - and each round is kept only when a re-check proves the file
  got better, so a speculative `did you mean` cannot rewrite working code.
- Report the real Cranelift and bytecode-VM outcomes in
  `gos test --tier-parity`. The walk invoked `gos FILE` rather than
  `gos run FILE`, so every fixture recorded `vm=fail`, and the Cranelift
  column was a copy of the VM's result rather than a JIT run.
- Decide `gos test --tier-parity` by comparing the tiers against each
  other - exit code and stdout - rather than against a zero exit. An
  example that deliberately aborts, or one given no argument, exits
  non-zero everywhere, and three tiers agreeing on that is the property
  the walk exists to prove; each was recorded as a failure on every tier.
- Record no verdict, rather than a failure, for a fixture still running at
  the per-tier budget, and honour `--timeout` for that budget. A server
  example runs until it is killed, so its modules were published as broken
  on every tier.
- Report a stack overflow as `GX0008` with exit 101 on every tier. The
  bytecode VM refused the call and exited 1 while JIT-compiled and native
  frames reached the guard page and aborted with a core dump, so the same
  program ended three different ways. The guard handler now reports the
  code the VM uses and exits with the fault status the other faults use;
  it still names the faulting address, which only it knows.
- Aggregate tier-parity evidence per stdlib module, keyed by the module
  paths a fixture imports, so `gos feature-status` can report what the
  walk proved. The sidecar recorded only fixture paths, which no feature
  row could ever match, so the column was unpopulated by construction. No
  evidence ships with the release yet; the column reads `(no test data)`
  until a walk has run.
- Answer `gos doc std`, `gos doc std::<module>`, and `gos doc
  std::<module>::<item>` from the stdlib manifest. Three diagnostics
  (`GR0005`, `GR0009`, and the unknown-export help) point at these commands;
  `gos doc` only ever read files, so following the help reported
  `file not found: std`.
- Accept inline `source` in place of a file path on the MCP `check`,
  `execute`, `fmt`, `doc`, and `lint` tools, so a snippet needs no file of
  its own, and add `lint` and `feature_status` to the tool table.
- Format every shipped `.gos` source canonically and gate it in CI: 254 of
  them disagreed with `gos fmt`, including trait method declarations
  carrying a trailing semicolon.
- Keep `STDLIB_MANIFEST_ITEMS` sorted, and say so when it is not. The
  table is looked up by binary search, so one entry out of order made
  every later entry unreachable and reported it as missing from the
  manifest.
- Run the manifest, resolver-table, and CLI-surface consistency tests in
  `quick-check.sh`. They live in the workspace test suite the script
  otherwise skips, and they are the gates an ordinary edit - a new stdlib
  module, a reworded argument help - is most likely to break.

## 0.47.0 - Visibility, pub(package), deadlock detection, silently dropped fix,
## keyword and constant default arg, goroutine scalability/fixes, 
## opaque nominal aliases, typeInfo over enums/generics, 
## must_use and allow(unused_result), other fixes

- Resume a parked goroutine on the thread it parked on. A wake that arrived
  in the window between arming and suspending re-queued the goroutine onto the
  shared injector, so a suspended stack could continue on another worker
  thread; every thread-local read taken before the suspend then resolved
  against whoever resumed it. A goroutine could delete another's registration
  on a channel, a mutex, or a wait group, and the value queued for the victim
  was consumed by a wake nobody was left waiting for - a fan-in over an
  unbuffered channel hung.
- Wake the unbuffered sender whose value was taken, rather than the
  longest-waiting one, so a sender that re-parks cannot absorb another's
  handoff.
- Name the parameters a call leaves unfilled (`GR0015`) instead of reporting
  an argument count. With names and defaults a call can supply the declared
  number of arguments and still miss a parameter, so `wow(b = 0)` now says it
  gives no value for `a` and which parameters may be omitted.
- Splice a parameter default and rewrite a named argument in the REPL, not
  only in a file. The REPL drives the front-end phase by phase and never ran
  the rewrite, so `fn wow(a: i64, b: i64 = 100)` then `wow(9)` reported a
  missing argument.
- Name an argument at a call site (`volume(depth = 4, width = 2)`) and give a
  parameter a constant default (`fn volume(width: i64, height: i64 = 2)`).
  Positional arguments come first, then names, in any order; a default is
  spliced into every call that omits it. Both are rewritten into the callee's
  declared order before type checking, so the calling convention is unchanged.
- Report a call that names a parameter no callee declares, names one twice,
  writes a positional argument after a named one, or names an argument on a
  method that several types declare with different parameters (`GR0013`), and
  a parameter default that is not a constant (`GR0014`).
- Report a deadlock instead of hanging. A channel operation that would block
  with no goroutine left running now stops the program with `all goroutines
  are asleep - deadlock!` and exit 101, instead of waiting forever. A pending
  handoff, a timer, a socket, or a blocking call all count as progress, so a
  working program is never reported.
- Infer a generic enum's type arguments from its constructor. `Tree::Leaf(1)`
  left `T` unresolved unless an annotation supplied it, which left the value's
  type unknown: two instantiations of one enum could not coexist in a program,
  a nullary variant would not unify with the type it belonged to, and native
  builds read the wrong variant from any method on such a value while the
  interpreter read the right one.
- Bind a generic enum's payload at the instantiation the scrutinee has, so
  `Tree<i64>`'s `Leaf(v)` binds `v: i64` rather than an unresolved type.
- Reflect enums, tuple structs, and generic instantiations with
  `typeInfo::<T>()`. It described named-field non-generic structs only; an
  enum or a `W<i64>` reported a missing internal name. An enum yields each
  variant with its payload spelling, a tuple struct yields its positions, and
  a generic type yields its fields with the arguments substituted in.
- Report `GR0012` for a `typeInfo::<T>()` with nothing to reflect, instead of
  an unresolved `__gos_typeinfo_T` the user never wrote.
- Format a type recursive through `Box` on every tier. `println!("{:?}", node)`
  rendered on the interpreter and failed the native build.
- Derive on a generic enum. `#[derive(Debug)]` and its siblings produced no
  usable method for one, so `{:?}`, `==`, and `clone` were unavailable on
  every instantiation.
- Reject formatting a generic type that has no `fmt` (`GT0062`), naming
  `#[derive(Debug)]`. Formatting one rendered on the interpreter and failed
  the native build, because whether a generic type's fields render depends on
  the arguments each instantiation supplies.
- Enforce `pub` on names reached through a `use`. A `use` binds an opaque
  import, and the visibility check never followed it, so any module could
  import and call another module's private functions, types, constants, and
  aliases. A private item named from outside the module that declares it, or
  from outside that module's descendants, is now `GR0008`.
- Enforce visibility on associated functions and on traits. A `Type::helper()`
  declared without `pub` was callable from any module, and a trait a module
  kept to itself could be implemented from outside it.
- Enforce visibility on struct fields. A non-`pub` field of a `pub` struct is
  unreachable from outside the declaring module, whether it is read, written,
  or named in a struct literal, so a type can be public while its
  representation stays private (`GT0065`).
- Add `pub(package)`, reaching every module of the declaring package and
  nothing beyond it. `pub(crate)`, `pub(super)`, and `pub(in path)` are
  rejected with a diagnostic naming it (`GP0038`).
- Type a value produced by an imported item. A call to a `use`-imported
  function of the same program, and a literal of an imported struct, both came
  back as an unresolved type, so nothing downstream of either was checked:
  assigning an imported function's `i64` result to a `String` compiled.
- Report a `Result` discarded as the value of an `if`, a `for`, or a `while`.
  Only a directly-discarded expression was checked, so `if ready() { flush() }`
  dropped the error with no diagnostic.
- Honour `#[allow(unused_result)]` on an item and `#![allow(unused_result)]` on
  a file. Both parsed and did nothing, leaving the escape hatch the language
  documents unavailable.
- Honour `#[must_use]` on a function, struct, or enum declaration. Discarding
  such a value is `GT0064`, so a guard or a builder can carry the same
  guarantee `Result` does.
- Call the `impl` method rather than a builtin of the same name on every
  tier. Beyond the interpreter, a native build routed `value.len()` on a user
  type to the sequence-length helper, which reads a `Vec` header out of the
  receiver's pointer: `len` and `map` on an enum returned garbage from
  compiled code while the interpreter returned the declared value.
- Call the `impl` method rather than a builtin of the same name. An enum value
  carries only its variant name at run time, so a user method named for a
  sequence combinator - `count`, `len`, `map`, `min`, `find` - reached the
  builtin instead: `count` answered 0 on the interpreter and the real value in
  a native build, with no diagnostic on either.
- Explain `GT0063` in `gos explain`. The code was emitted for a private method
  but had no registry entry, so the command had nothing to say about it.
- Declare a distinct type over an existing representation with
  `type UserId = new i64`. It converts to and from its representation with
  `.into()`, inherits equality, ordering, hashing, and formatting, and
  inherits neither the representation's methods nor its operators.
- Keep `new` when formatting a type alias. `gos fmt` wrote the target without
  it, so formatting a file turned every opaque alias into a transparent one.
- Serialize a struct field whose type is written as an alias. Both forms were
  rejected as unserializable, which made `struct User { id: UserId }` - the
  case an opaque alias exists for - underivable.
- Spell a serde turbofish target through an alias: `to_json::<Record>(v)`
  where `type Record = Rec` uses the target struct's codec.
- Report which shape a serde turbofish named when no codec exists for it
  (`GP0039`): a generic struct, an enum, or a name that is not a struct. Those
  three reported only a synthesized internal name the user never wrote, and a
  struct with an unserializable field reported that name after `GP0022`.
- Name a callable field's type when reporting an underivable struct. It
  rendered as `_`, so the report identified nothing to act on.
- Reject a `for` loop over a `Result` or an `Option` (`GT0067`). Neither is a
  sequence, so the loop bound nothing, ran zero times, and left the binding
  unconstrained - `for entry in fs::read_dir(dir)` compiled, read fields off
  `entry`, and silently did nothing.
- Promote `std::fs` and `std::env` to shipped, and record the programs that
  exercise them. `gos feature-status` and the stdlib coverage table now name
  the fixture behind an item instead of reading `not item-audited` for every
  entry regardless of what was run.
- Add a tour lesson for named arguments and constant parameter defaults, and
  fix three lessons whose forward-pipe samples still used `_` as the
  placeholder instead of `$`, which no longer runs.
- Build the browser playground again. The deadlock-reporting hooks were added
  to the native scheduler only, so `gossamer-runtime` stopped compiling for
  wasm32 and the docs site could not be published.
- Grow the interpreter's goroutine pool on demand. It was fixed at four
  threads, and a goroutine blocked in a channel operation holds its thread, so
  four blocked goroutines starved every goroutine still queued - including the
  sender whose value would have released them. A worker pool of four or more
  receivers fed by a goroutine hung.

## 0.46.2 - Arena slab retention, library impls, method visibility

- Bulk-free a sequence combinator's per-element allocations. A closure body
  driven by `map` / `filter` / `sum` and their siblings now takes the same
  automatic arena region a loop body does when the value it returns cannot
  point into that region, so `xs.map(|x| build_and_discard(x))` runs as fast
  as the `for` loop spelling the same iteration instead of paying a per-node
  reference-count teardown.
- Report a path dependency's files under the path its manifest spelled. Origins
  came back resolved through symlinks and, on Windows, in verbatim `\\?\` form,
  so a diagnostic raised inside a dependency named a file the user had not
  written.
- Reach the methods of a type declared inside a module. The type registered
  under its module-qualified identity while its `impl` keyed methods by the
  bare name, so a library's own `pub` method calling a private helper was
  reported as missing, and a trait implemented for such a type failed a
  `T: Trait` bound.
- Run an associated function's body instead of packing its arguments. On the
  interpreter and JIT, a two-argument call on a two-integer struct compiled as
  a positional construction, so `Point::new(a, b)` produced a struct built from
  the arguments and never entered the function. Native builds ran the body, so
  the tiers disagreed on the result.
- Bind a bare `Type::assoc()` written inside a module. The body is keyed by its
  module-qualified name, so the call reported an unbound name at run time and
  failed the native build.
- Reach a dependency's associated functions through `use "id" as alias`. Only
  free functions resolved through an alias; `alias::Type::assoc()` was rejected
  as an unresolved name.
- Type a call written as `module::Type::assoc(..)`, so a method call on its
  result is checked. Only the unqualified two-segment spelling was recognised,
  which left the value untyped and every use of it unchecked until run time.
- Print a struct or enum through the bare print builtins on every tier.
  `println(value)` rendered on the interpreter but failed the native build;
  only the `println!("{}", value)` form routed an aggregate through the
  formatting its type derives.
- Report a diagnostic against the file that declares the code. Path
  dependencies and sibling modules are assembled into the entry file and every
  span resolved to that file, so an error inside a library pointed at a line
  the entry file did not have.
- Enforce `pub` on a method. Visibility was dropped when an `impl` item was
  parsed, so a method without `pub` was callable from anywhere its type could
  be named, including another project. A method now follows the rule a free
  function already did: the declaring module and its descendants reach it, so a
  `pub` wrapper still calls the private helpers beside it.
- Keep as many arena slabs warm as a thread's widest region actually used.
  A fixed four-slab cache decommitted the rest at every region close and
  faulted them back in at the next open, so a region-heavy program paid five
  times the page faults and three times the system time for memory it
  immediately reused. Retention now follows the measured width, bounded by a
  ceiling, and a thread whose regions stay narrow holds fewer slabs than the
  fixed cache did.
- Allocate goroutine ids from one counter. The diagnostic registry and the
  scheduler each handed out ids into the same process-wide table, so a
  finishing goroutine could remove a live goroutine's entry from a stack dump.

## 0.46.1 - String accumulation, compiled maps, scheduler overhead

- Stop freeing a string that `+=` is still building. The append helpers take
  ownership of the accumulator and hand it back, but only some of them were
  treated that way, so `s += "text"` released the buffer the call had just
  returned and every later append read freed memory. Building a string in a
  loop was quadratic as a result: four million appends now take less time than
  eighty thousand did.
- Refuse a goroutine the host cannot give a stack, instead of aborting. A
  stack costs two mappings, so a process reaches `vm.max_map_count` at roughly
  32 thousand live goroutines; that now surfaces as a declined spawn.
- Poll for preemption without a write. The check runs on every loop back-edge
  and performed an atomic read-modify-write even when nothing was pending.
- Remove a registered I/O source in constant time. Each completed read scanned
  the poller's whole token table to find the entry to drop.
- Compile a function that builds a map. Holding a `Map` local kept a function
  on the interpreter however hot it became; one that keys and values by
  integers, `bool`, `char`, or `String` now compiles, running a map-building
  loop about 2.4 times faster.
- Compile a map literal. `{"k": v}` had no native lowering, so a function
  using one fell back to the interpreter even when it was otherwise eligible.
- Drop an unreachable check from every reference-count retain. The path tested
  whether an already-untagged pointer was a string, which its address shape
  rules out.
- Copy a packed sequence across the compile boundary in one move. An integer
  vector, a float vector, and a byte buffer were handed to compiled code one
  element at a time.
- Index an ASCII string without building an index. Every allocation and every
  append that grew a string walked its contents to record character offsets,
  which for ASCII are the byte offsets; building a string is about 2.8 times
  faster. Text outside ASCII keeps the full index.
- Stop serializing string work across goroutines. Appending to a string took a
  process-global lock to identify the accumulator, and allocating or releasing
  any heap string took the same lock, so concurrent string building on
  different goroutines contended on one mutex. The append path now reads the
  accumulator's own type tag, and the registry behind the remaining raw-pointer
  entry points is sharded by address.
- Serialize JSON in linear time. Each fragment written to the document rebuilt
  the accumulator's character index from the first byte, so encoding cost grew
  with the square of the output size.
- Store an integer `Set` as integers. `Set<i64>` and `BTreeSet<i64>` kept every
  element as decimal text, so each insert, lookup, and removal formatted a
  string and allocated, and a live element cost roughly twice the memory.
- Drain goroutines when `main` returns nothing. A native build of a program
  whose `main` has no return value exited without waiting for goroutines still
  running, losing their output; the bytecode VM and the JIT both waited.
- Remove dead code from a build that keeps debug info. `--gc-sections` was tied
  to symbol stripping, so `gos build -g` linked the entire runtime surface
  including the unused HTTP, TLS, and compression stacks.
- Release a goroutine's diagnostic record when it finishes. Every spawned
  goroutine left an entry in the stack-dump registry for the life of the
  process, and the lookups that registry serves slowed as it grew.
- Skip the goroutine bookkeeping a compiled program never uses. Binding a
  goroutine to a worker took a process-global lock twice per scheduling step to
  migrate call frames that only the interpreter records.
- Count an aggregate allocation once. Allocating through the aggregate entry
  point incremented the live-object ledger twice, and every such allocation
  paid an atomic read-modify-write that only the MSVC link path needs.
- Reduce interpreter and runtime memory: the type-tag table no longer rescans
  itself on every optional or fallible value a builtin constructs, method calls
  reuse pooled argument storage, the cycle collector's root buffer uses a
  pointer-appropriate hash, and a thread caches 4 arena slabs instead of 64.

## 0.46.0 - Explicit imports, hashable map keys, Memory optimizations

- Require an import to name a module's items. A sibling file's `pub fn add`
  is reached as `util::add` or after `use util::add`; a bare `add` no longer
  resolves through a flat unit-wide namespace. `GR0011` names the declaring
  module and the exact `use` line to add.
- Key a `Map` or `Set` by any hashable value on every tier: integers, `bool`,
  `char`, `String`, tuples, fixed arrays, structs, and enums - unit variants
  and payload variants alike - nested freely. Keys compare by content, so an
  equal key built at a different allocation finds the same slot.
- Return the key a program wrote from `keys()` on an aggregate-keyed map.
  `Map::keys()` was rejected outright for struct, tuple, and array keys, and
  `for k in m.keys()` read a multi-word key as a single scalar and faulted.
- Support `remove`, `pop`, `get_or`, `or_insert`, and `inc` on an
  aggregate-keyed map in compiled builds. Only `insert` / `get` /
  `contains_key` were routed, so `remove` silently missed and `inc` failed to
  link.
- Key an enum-keyed map by discriminant and payload rather than by node
  address, so `m.get(Slot::Taken(3))` finds the entry a distinct but equal
  node inserted.
- Let two modules declare the same type name. A type's identity below the
  resolver is now the module that declares it, so `a::Point` and `b::Point`
  keep their own fields, `{:?}`, `==`, map keying, serde symbols, and impl
  methods; a rendered value still shows the name as declared.
- Name a struct literal's type by its declaration, not by the alias it was
  reached through. `use a::Point as P; P { .. }` rendered as `P { .. }` and
  failed to link in a native build.
- Keep the enum behind a unit variant's value. `{:?}` and `==` on a bare
  variant of a payload-bearing enum lost the type and printed the raw
  discriminant in compiled builds.
- Isolate the runtime staticlib build from the outer `RUSTFLAGS`. The build
  script dropped `RUSTFLAGS` but not `CARGO_ENCODED_RUSTFLAGS`, which cargo
  actually sets, so workspace codegen flags leaked in and could fail the
  inner build. Dropping the leak surfaced two `-C` flags rustc has never
  accepted, which the leak had been masking; they are gone.
- Link the released binaries at a fixed load address. A position-independent
  `gos` wrote  relocations into private dirty pages before `main`
  ran; resident memory now starts about lower - at identical execution speed.

## 0.45.2 - String index spaces

- Make directory modules reachable at any depth. The bundler declared each
  module the project layout implies without `pub`, so anything under
  `src/<dir>/<dir>/` was private to its parent with nowhere to write the `pub`
  that would open it; `deep::nest::item` was unreachable from the entry.
- Anchor module-relative paths at the module that writes them. `self::child::f`,
  a bare `child::f`, and `super::sibling::f` had their prefixes stripped and
  were looked up from the unit root, so a module could not call into its own
  child - `gos check` passed and the call failed at run time with `GX0002`.
- Resolve a struct declared inside a nested module. Its synthesized structural
  `eq` / `cmp` are spliced at the unit root, and the visibility check rejected
  them against the type's module, failing the build for a program that never
  named the type.
- Name the private module, not the item, when a module on the path is what
  blocks a reference. `GR0008` told the user to write `pub` on a function that
  already had it.
- Reject `mod name;` with no module source behind it (`GR0010`). Outside a
  project the layout is never read, so the declaration bound nothing and the
  call failed at run time with `GX0002`.

- Correct the `substring_byte_scan` (`GL0052`) suggestion. It offered `s[i]` as
  the byte behind `s.substring(i, i + 1)`, so following the lint produced
  `GT0001` on `s[i] >= b'0'`; `s[i]` is the `char` at a character index, while
  `substring` takes byte offsets. The lint, its `explain` text, and the
  `GL0052` catalogue entry - which described an unrelated lint about string
  search methods - now name `s.byte_at(i)`, which reads the same byte offset as
  an `i64` and preserves the scan's behavior on non-ASCII input.
- Document the two String index spaces in the skill card. `len`, `[]`, and bare
  iteration count Unicode scalars; `byte_len`, `substring`, `byte_at`,
  `as_bytes`, and `bytes` take byte offsets. The card previously described
  `s[i]` as the byte as an `i64`, which no tier accepts.

## 0.45.1 - Handles, containers, and captured parameters

- Reject formatting a value that has no text form. A runtime handle
  (`http::Client`, `sync::Map`, `bytes::Builder`, `flag::Set`, `io::Stream`,
  `context::Context`, `metrics::Counter`, `trace::Tracer`, `rand::Rng`,
  `http::Router`, `http::Response`, `validate::Errors`, a middleware
  `Handler`), a function or closure value, and a channel endpoint or join
  handle now report `GT0062` at check time. A native build printed a handle's
  raw address - a different number on every run of the same binary - while the
  VM printed a reflective dump of its private fields, and formatting a callable
  or an endpoint reached the backend as an "internal lowering bug". Each handle
  carries a named type so the diagnostic names what was formatted and what to
  print instead.
- Print `Deque`, `Queue`, `Stack`, `MaxHeap`, and `MinHeap` the same way on
  every tier. Compiled tiers printed the container's address, or refused the
  build when the container was bound to a name, and the VM rendered a heap as
  an internal registry id. All three now render through one runtime shim per
  container: `Deque [0, 1, 2]`, `MaxHeap [9, 3, 5]`.
- Render `Set` and `BTreeSet` under the Cranelift JIT, which printed the set's
  address. String elements print in their Display form on every tier, matching
  how a sequence of the same elements prints.
- Resolve a module-qualified built-in type name: `collections::Deque<i64>` in a
  parameter or return position resolved to nothing, because a built-in name was
  only recognised as the first segment of a path. A container-typed parameter
  was therefore untyped, and the compiled tiers passed the handle in a slot the
  callee read as a pointer, so `fn size(q: collections::Queue<i64>)` returned
  garbage. Containers and handles now occupy one i64 slot in every position.
- Fix a closure that captures a `Vec` or slice parameter. The capture's storage
  claimed a register inside the parameter block, so every parameter declared
  after the captured one received the wrong argument, or none at all: a
  function whose closure captured a sequence parameter computed a wrong result,
  or printed `<void>`, on the bytecode VM while the compiled tiers were
  correct.

## 0.45.0 - Pipe placeholder, sound generics, wired stdlib

- Ship associated types and associated constants. A trait declares `type Item`
  and `const MAX: i64` with optional defaults, each `impl` supplies one, and
  `Self::Item`, `T::Item`, `Type::MAX`, and `T::MAX` resolve through the impl,
  the trait default, or the trait's single implementor. `T: Iterator<Item =
  i64>` now parses and pins the projection. Both were parsed and resolved but
  discarded before lowering, so a projection silently read as the base type and
  an associated constant faulted at runtime. New diagnostics name an impl that
  omits a required associated item (`GT0059`), a projection nothing declares
  (`GT0060`), and an ambiguous projection with the constraint to write
  (`GT0061`). Generic associated types and associated items on `dyn` remain out
  of scope.
- Skip the whole front end on a repeat `gos check` / `gos run` / `gos test` /
  `gos build` over an unchanged program. The cache now stores the resolved and
  typechecked result, not just the parse, and invalidates on source, imports,
  compiler identity, edition, target, `#[cfg(test)]` visibility, and binding
  signatures. Blobs live in the project's `.gos-cache/frontend`; `GOS_NO_CACHE`
  turns the cache off.
- Spell the pipe placeholder `$` instead of `_`: `text |> $.trim()` makes the
  piped value the receiver, and `x |> f($, k)` selects the argument it fills.
  `$` is punctuation, so it can never collide with a name in scope. A pipe
  written with the retired `_` reports `GP0038` naming the new spelling.
- Call a trait method through the type parameter of a bounded generic `impl`
  block. `impl<T: Shape> Wrapper<T> { fn area(&self) -> f64 { self.value.area() } }`
  ran on the bytecode VM and crashed a native build: the method copied the
  receiver out of its slot, while the impl it resolves to takes `&self`, so the
  callee read a struct value as a pointer. The generic template is also no
  longer emitted once every call site routes to a specialisation, which left an
  undefined symbol for the unresolved trait call.
- Give every slot of a repeat literal its own element. `#[#[0; 3]; 3]` built one
  inner Vec and stored it in all three slots on the compiled tiers, so writing
  `c[0][0]` changed every row. The per-slot clone was also being discarded by an
  optimisation that counts static reads, which cannot see that a clone written
  once inside a loop runs once per iteration.
- Parse a const generic argument as a literal or a braced block rather than a
  full expression. `f::<3>(xs)` read `3 > (xs)` as a comparison and never closed
  the argument list, so only the `f::<3,>(xs)` spelling reached the checker.
- Call the HTTP middleware stack from Gossamer. `cors`, `rate_limit`, `timeout`,
  `recoverer`, `compress_gzip`, `body_limit`, `hsts`, `security_headers`,
  `cache_control`, `etag`, `logger`, basic and bearer auth, and `safe_defaults`
  were implemented but reachable only from Rust; they now compose with `Chain`
  and `http::serve`, with configuration types for CORS, HSTS, security headers,
  cache control, and rate limiting.
- Add `path::glob`, which walks `**`, and `path::matches`.
- Add `std::sort` with `sort_stable`, `binary_search`, and `partition_point`
  over integer, float, and string elements.
- Add the `io` streaming adapters: `tee_reader`, `multi_reader`, `limit_reader`,
  `pipe`, `copy_n`, and in-memory readers and writers.
- Match an error by identity. `errors::is` now accepts a sentinel error value
  and compares it through the cause chain, alongside the existing message
  match, and `chain`, `with_field`, `field`, and `fields` are callable.
- Observe a closure's mutation of a captured `Vec` from the enclosing binding on
  the bytecode VM, as the compiled tiers already do. The VM snapshotted the
  capture into the closure's own copy-on-write share, so `let add = || v.push(2)`
  left `v` untouched. A captured sequence now lives in one shared cell that the
  binding and the closure both name, mutations travel both ways, and assigning
  the whole variable still gives the binding its own storage.
- Iterate `String::bytes()` in a native build. It ran on the bytecode VM and
  emitted an undefined symbol under LLVM AOT.
- Sort a `Vec<f64>`. The comparison read float bits as string pointers and
  crashed a native build.
- Compare an error message the same way on every tier: the bytecode VM required
  an exact match where the compiled tiers matched a substring.
- Read a tagged-pointer enum through the combinator surface, through a closure
  argument at a direct call site, and read a generic struct's fields from its
  per-instantiation layout. Each read a slot address where the value's handle
  belonged, so `.map()` over a `Vec<Enum>` always matched the first variant and a
  `Wrapper<Point>` in a loop summed the wrong fields.
- Accept a plain function as an `http::middleware` handler in a native build.
  Only a type with a `serve` method was resolved, so wrapping a function
  type-checked and then failed to link.
- Build a runtime error message through the runtime's own allocator. The HTTP
  client handed the error constructor a pointer with no length header, so
  reading the header read outside the allocation.
- Carry a string's own length across the C ABI rather than stopping at the first
  NUL byte. A `String` holding an interior NUL was truncated on the compiled
  tiers while the bytecode VM kept it whole; the remaining reads are of
  host-owned C strings (`argv` and a native binding's pointer), each documented
  and held to that by a test.
- Produce a bit-identical artifact from `gos build --reproducible`, including
  across build directories and with the compilation caches live.
- Stop reporting a used path dependency as an unused import. `use
  "example.com/intcode"` was matched against the whole project id while call
  sites spell the last segment (`intcode::item`), so the correct import always
  warned. An import that really is unused still does.
- Keep a `for` loop's counter separate from the binding that supplied its start.
  `for x in lo..=hi` advanced `lo` itself, so `lo` held the end value afterwards
  and re-entering the loop from an enclosing one started past the end and ran
  the body zero times. Literal bounds were unaffected, which is why the failure
  looked like it depended on an unrelated `.rev()` elsewhere in the program.
- Run an `iter` combinator over a range whose result element the lazy iterator
  runtime does not carry. A range produces a lazy handle, and a `char`, `bool`,
  or float result falls to the eager path, which read that handle's words as a
  vector header and dereferenced a null pointer in a native build. Closes the
  crash reported for a program that ran on the bytecode VM and segfaulted once
  compiled.
- Infer the payload of `Option::map` / `Result::map` from the closure's return.
  The closure's parameter was unconstrained when its body was checked, so a
  projection out of the payload stayed a free variable that later unified with
  whatever the context wanted; `some_pair.map(|p| p.0)` then printed an integer
  through the string path and crashed a native build.
- Let a closure observe the value it captured rather than a clone of it, which
  matches the documented capture-by-managed-reference rule. Mutating a captured
  `Vec` was a no-op, and a captured nested `Vec` reached the compiled tiers with
  element metadata that no longer described the original buffer.
- Stop reading an `Option` or `Result` payload word as a pointer when it holds a
  scalar. The reference-count probe dereferenced the word to look for an
  allocation header, so `Option<i64>` and friends read an arbitrary address; it
  faulted only when the integer happened to be misaligned, which made the crash
  look specific to one program.
- Type the closure parameter of `option::map` from the option's payload rather
  than assuming an integer, so a destructuring closure over a tuple payload sees
  its real shape.
- Snapshot a lazy iterator source into a `Vec` before an eager combinator reads
  it. `(0..n).map(f)` whose element or result type keeps the chain off the lazy
  path handed the range's iterator state to a shim that indexes a `Vec`, so a
  native build read the handle's words as a vector header and crashed.
- Honour a `where` clause. Its predicates were parsed, name-resolved, and then
  dropped, so `fn f<T>(x: &T) where T: Shape` reported that `T` had no methods.
- Read the bounds of a generic `impl` block inside its methods, and keep bound
  positions aligned when the block and the method both declare parameters: a
  method's own bound could be attributed to the block's parameter instead.
- Check declared bounds on struct, enum, trait, and impl generics. An unknown
  trait in `struct S<T: Hashabel>` was accepted, and an unsatisfied bound was
  never reported when the type was instantiated.
- Reject an operator applied to a type parameter that no bound supports, the way
  a method call already was, and enforce a bound naming an operator trait.
- Report a trait impl that leaves out a method the trait declares without a
  default body (`GT0058`). Such an impl satisfied a bound and then lowered to a
  call to a symbol that was never emitted.
- Decide match exhaustiveness with the usefulness algorithm over a pattern
  matrix. Tuple, fixed-array, and nested payload scrutinees were treated as
  exhaustive whatever the arms covered, so `match (a, b) { (true, true) => 1 }`
  type-checked and panicked at run time; the missing combinations are now
  reported as witnesses.
- Bind a struct-variant or struct pattern through a reference the way a tuple
  variant already did. `Shape::Rect { w, h }` matched against `&self` left the
  bindings as references and failed to type-check against their field types.
- Type the return of a method resolved through a type-parameter bound. An impl
  block's self type was lowered outside its own generic scope, so `Wrapper<T>`
  never recorded its parameter and a `String` return printed as its pointer in a
  native build.
- Correct the declared signatures of `encoding::binary::uvarint` and `varint`,
  which returned a pair at run time and a single integer in the type table.
- Bind the suffix of a tuple rest pattern from the end: `(a, .., d)` over a
  four-element tuple bound `d` to the second element.
- Construct a module-qualified enum variant on the compiled tiers.
  `shapes::Shape::Circle(1.0)` from outside the module ran on the bytecode VM
  and emitted an undefined symbol in a native build, because variant lookup
  matched only one- and two-segment paths.
- Report the cause chain when a native build fails, instead of the outermost
  message alone.
- Fold a constant `u64` or `usize` comparison at its own signedness. Operands at
  or above 2^63 are stored sign-extended, so `u64::MAX > 5` folded to `false`
  while every execution tier computed `true`.
- Document trait bounds as written: several bounds per parameter (`T: A + B`)
  and `where` clauses both work, and the reference described only single bounds.
  Correct the HTTP/2 and WebSocket status, and the scheduler's unpark path,
  which pins a goroutine to its home worker rather than migrating it.
- Pass `Result` and `Option` carriers to the runtime under the Win64 `extern
  "C"` ABI on Windows. The JIT declared them as an integer-register pair while
  the call site marshalled them by pointer, so any hot body reaching one of
  those helpers failed to compile and the program ran wholly on bytecode.
- Keep native compilation for the rest of a program when one body cannot be
  lowered, rather than dropping the whole module to bytecode, and JIT-compile a
  nested enum payload such as `Some(Some(v))` instead of rejecting its body.
- Treat an already-absent cache directory as cleaned by `gos clean`, which
  failed when a concurrent build removed one first.
- Split paths at either separator on Windows, where the OS hands back `\`:
  `path::file_name`, `parent`, `split`, `components`, and `is_absolute` read a
  whole `C:\dir\file.gos` as one component, so `path::glob` results could not
  be reduced to their basenames.
- Produce a byte-identical binary from `gos build --reproducible` on Windows.
  The PE header carries a wall-clock timestamp, so every link differed; the
  reproducible link now derives that field from the image contents.
- Resolve the retired build-cache root from `USERPROFILE` on Windows when
  `HOME` is unset, instead of sweeping the current directory.

## 0.44.1 - Iterator elements, Doc fixes.

- Accept an `Iterator<T>` argument for an `Iterator<T>` parameter. Passing
  `v.iter()` to a function taking `Iterator<i64>` reported a mismatch whose
  expected and found types were both `Iterator<i64>`.
- Run `.iter()` chains over every element type on the compiled tiers. `.iter()`
  built one iterator handle while the adapters and terminals consumed another,
  so `.count()`, `.skip()`, and `.collect()` over a `String`, `f64`, or struct
  element faulted natively, and `.next()` failed to link.
- Return `iter::min` and `iter::max` over a `Vec<f64>` as floats. They reported
  the integer their bits spell, and now order through `total_cmp` so a NaN
  cannot silently win the comparison.
- Bind a `String` from `xs.iter().next()` as a `String` on the compiled tiers
  rather than as its raw pointer.
- Visit every entry of `fs::walk_dir` and `path::walk` in a native Windows
  build. The visitor's `Result` came back in the wrong register, so the walk
  ended after the first entry.
- Correct the Vec and fixed-array literal spellings in the methods-by-type and
  sequence-safety references: `#[a, b]` creates a `Vec<T>` and `[a, b]` creates
  a fixed `[T; N]`. Both pages said the reverse.
- Point `vec![...]` at `#[...]`. The diagnostic steered Rust habits to `[...]`,
  which builds a fixed array that then rejects `push`.
- Offer Vec completions for `#[...]` and fixed-array completions for `[...]`.
  The editor had the two literal forms swapped.
- Name `<expr>.iter()` in the help for a `Vec<T>` passed where an `Iterator<T>`
  is expected, instead of describing the fix in the abstract.
- Accept a declared name in the REPL's `%drop`, which removes the declaration
  that introduced it together with the declarations that name it. Redeclaring a
  name is rejected as a duplicate definition, so this is what frees it.

## 0.44.0 - Authoritative trait bounds, diagnostic clarity, stdlib + other fixes

- Name the fix for a `const` or `static` written without a type: `const y = 1e-12`
  reported three errors, one of them about an empty name, and now reports
  `GP0034` alone with `y: f64` as an applicable edit.
- Render REPL diagnostics through the frame `gos check` and the LSP already use,
  so one mistake reads identically everywhere; the REPL previously dropped every
  error code, note, help, and suggestion.
- Stop reporting names the parser invented while recovering. A failed name became
  an empty or `<error>` spelling that later passes reported as missing, and whose
  "did you mean" proposed replacing a name with itself.
- Skip serde and derive synthesis when the source did not parse. Generated code
  built from a recovered tree was reported against line numbers past the end of
  the user's file; one unclosed brace produced 28 diagnostics, now one.
- Withhold a "did you mean" whose edit distance reaches the length of the name it
  corrects, and never propose a candidate identical to it.
- Keep a missing stdlib import from also suggesting an unrelated name: the fix for
  `iter::sum(v)` offered to rewrite it as `str::sum(v)`.
- Draw "did you mean" from the scope where the name failed, so a mistyped local,
  parameter, or closure binding is corrected; candidates were top-level item
  names only, leaving the most common typo with no hint at all.
- Name the module a misspelled `use` meant: `use std::json` now points at
  `std::encoding::json` instead of restating that the path does not exist.
- Print a suggested edit once rather than repeating it beside itself.
- Drop the note under a type mismatch that restated the title verbatim.
- Give `GP0001` a help line. The parser's most common error was its only one
  with no guidance attached.
- Resync to the closing delimiter after a bad token in a parameter list,
  argument list, or generic list. A missing comma in `fn add(a: i64 b: i64)`
  reported 9 parse errors and now reports one.
- Say what was expected in the reader's terms: "a type name" rather than
  "path segment identifier", the actual item keywords rather than "item
  keyword", and the macro being parsed rather than "macro invocation".
- Name `Vec` and `Set` in the `#` literal errors, which described `#[..]` as a
  fixed array and `Set` as a hash set.
- Report a refutable `let` written without its `else` block (`GP0037`); the
  tail parsed as a struct literal and surfaced later as an unrelated error.
- Report a repeated `..` in a slice pattern (`GP0035`) and a repeated `..base`
  spread in a struct literal (`GP0036`) against the offending token.
- Suppress the statement-separator error once the same statement has already
  reported one; it was the terminal noise line of nearly every cascade.
- Stop reporting a type that already failed to check. A nested unsized slice
  produced follow-on errors naming `<error>`, a spelling absent from the
  source; one stale program dropped from 190 diagnostics to 147.
- Name the field or variant a typo meant, and list what the type declares,
  rather than advising the reader to check that a definition just resolved is
  in scope.
- Suggest the method a receiver does carry: `p.nrom()` names `norm`, and
  `v.lenght()` names `len`.
- Explain an integer-to-float mismatch with the cast to write, and an
  `Option<T>` used as `T` with the unwrap or `if let` to write.
- Drop the advice to call `.as_str()` on a `String`, which has no such method.
- Stop advising a cast to `i128` or `u128`, which the language rejects.
- Point the unused-`Result` diagnostic at the call rather than the enclosing
  block's closing brace.
- Report a repeated import only when one name is bound to two different
  paths; the same path imported twice is a lint, not an error.
- Run exhaustiveness, arena-escape, and comptime checks in the LSP, which
  reported none of them, and report generated-code failures against the
  declaration that caused them instead of dropping them.
- Run the default lints in `gos check`, which editors already reported.
- Fail an MCP tool call when the command it ran failed, and return parsed
  diagnostics rather than a text blob.
- Give `Range<T>` the type a range expression produces. The name was in scope
  but bound to nothing, so `fn f(it: Range<i64>)` type-checked and then
  iterated zero times, silently returning 0 where 10 was correct.
- Check a trait bound against its impls whenever the trait is declared in this
  unit, even when its name matches a built-in one. Declaring `trait Ord` turned
  off bound checking for every `T: Ord`, so an unrelated type was accepted.
- Retire the `redundant_field_init` lint, which advised the field shorthand
  `Point { x, y }` that named-struct construction rejects.
- Read `#{a, b}` as a `Set` literal wherever an expression is allowed. At the
  start of a block or as a tail expression it was taken for an attribute and
  rejected as `GP0013`.
- Remove `Atomic` from the prelude. It named no type anywhere in the compiler,
  so `let x: Atomic = 5` type-checked and any use failed at run time; the
  atomic types are `AtomicI64`, `AtomicI32`, `AtomicU64`, and `AtomicBool`.
- Resolve a method on a generic parameter through its trait bounds alone
  (`GT0056`). A call no bound declared bound an unrelated type's method body
  and read the receiver at that type's field layout, which type-checked
  cleanly and printed values built from the wrong fields.
- Require a bound that provides iteration to write `for x in t` over a generic
  parameter. The loop otherwise lowered against whatever shape each
  instantiation happened to have, yielding zero iterations on the VM and a
  fault in a native build.
- Walk a generic parameter through the `next` protocol rather than an indexed
  sequence read, so a user-defined iterator passed to a generic gives the same
  answer on the VM, the JIT, and a native build.
- Gate the type system on an adversarial suite covering arguments, numeric
  conversion, references, collections, aggregates, and trait bounds.
- Keep a value alive for as long as a `Weak` can reach it. `w.upgrade()` read
  `None` for a referent that was still in use, and a native build faulted.
- Reject a `use` naming an item its module does not export (`GR0007`), which
  passed as an unused-import warning and failed at run time.
- Import `Mutex`, `WaitGroup`, and the other `std::sync` primitives by name
  rather than reading them from the prelude, so a bare `Mutex` reports the
  missing import instead of binding a name that accepted any value.
- Rebuild the musl runtime archive for every profile. `gos build --release`
  links it whatever profile `gos` itself was built in, so a runtime change was
  invisible to native builds until `gos` was rebuilt in release.
- Build a release binary for a struct with no fields, which emitted a `void`
  parameter into its generated serde body and failed to compile.
- Lower `gos_rt_enum_box_aggr` on the JIT, which bailed the whole enclosing
  body back to bytecode.
- Document `GR0005`, which the generated diagnostics page omitted.
- Reject passing a built-in iterator to a parameter bound by an iteration
  trait (`GT0057`), pointing at the parameter spelling every tier lowers. The
  call reached the native backend as an internal lowering failure.
- Answer `m.lock()` with the `()` its documentation states. It handed back the
  guarded value on the VM and nothing on a native build, so the same source
  read differently per tier.
- Report a range as `Range<i64>`, the type written, rather than the internal
  `Iterator<i64>`. A range converts to the iterator it advances through, and
  only in that direction, so an adapter chain is still not a range.
- Type a closure argument from the callback slot it fills. A signature naming
  `Fn(..)` left the closure's parameters unresolved, so a field read inside the
  body took the dynamic path and produced bytes rather than the field on a
  native build; `fs::walk_dir` and `path::walk` now visit identically on every
  tier.
- Resolve a stdlib handle named in a signature to the same type a written
  annotation gets, so `fs::DirInfo` carries its fields either way.
- Type `fs::walk_dir` from its declared signature rather than pinning it to the
  listing form's return type, and give `path::walk` the signature it lacked.
- Bind every nested stdlib function under its leaf-module spelling, so
  `use std::compress::gzip` plus `gzip::encode(..)` runs on the VM the way it
  already built natively; 158 functions type-checked and then failed at
  runtime with `GX0002`.
- Fold a leaf-module path to its canonical stdlib path in MIR dispatch, and
  gate both properties so the two spellings cannot drift apart again.
- Answer the `json::Value` query surface on a method receiver: `doc.get(k)`,
  `doc.keys()`, and the `as_*` casts returned `None` or an empty list on the
  VM and reached an undefined symbol in a native build.
- Return `Option` from the JSON casts on the compiled tier: `v.as_i64()` gave
  `Some(1)` on the VM and a bare `1` natively.
- Drop the duplicate `compress::gzip` registration whose String-payload
  implementation shadowed the byte-payload one depending on install order.
- Type `String::from_utf8` so `?` sees its `Result`.
- Fix the rotted web-service load harness and the three struct-literal probe
  programs, which no longer compiled.
- Build a `Set` from a sequence value, not just a literal list: `Set::from(v)`
  reached an undefined `Set` symbol in a native build.
- Name the replacement for a renamed container wherever it appears, not only
  in a `use`: `HashSet<i64>` in a signature reported a plain missing name.
- Lower qualified `Vec::insert` / `Vec::remove`, which a canonical-path rewrite
  left unmatched so both spellings emitted an undefined symbol and a native
  build failed at its LLVM symbol audit.
- Search a sequence with the language's own structural `==`: `contains`,
  `index_of`, and `count_of` compared one raw slot, so on the compiled tiers an
  `f64` needle truncated to an integer and every struct, tuple, enum, and
  nested sequence compared by address.
- Report the compared values from `assert_eq` on the compiled tiers, which
  printed only the supplied message, and compare its operands structurally
  rather than by address.
- Pass a struct or `f64` element to `any` and `position` in its own shape; a
  struct predicate faulted in a native build and an `f64` one read the bit
  pattern as an integer.
- Hand a one-field struct's Vec element to the JIT by slot address like every
  wider struct, instead of pushing the pointer to its backing storage and
  reading that back as the field.
- Settle the process-global scheduler before asserting it is idle, so the
  check no longer races the sibling tests sharing it.

## 0.43.0 - Tuple surface, one name per container, collection literals,
## Iterator for-loops, reverse fix, dead method audit, explicit module imports

- Walk a `for` header's iterator source once on the bytecode VM: a range,
  `.rev()`, or an adapter chain over either was rebuilt on every pull and
  repeated its first element forever.
- Lower `.rev()` on an iterator receiver in MIR; the compiled tiers emitted a
  call to an undefined `rev` symbol and failed to build.
- Keep `.rev()` lazy on the VM when its source is, so the reversed pipeline
  still answers `next()` instead of restarting at element zero.
- Snapshot a lazy `enumerate()` for-loop instead of indexing iterator state as
  a buffer, which faulted in a native build.
- Reject the sequence surface on iterator, `Map`, and tuple receivers:
  `reverse`, `sort`, `push`, `len`, `to_vec`, and friends were accepted and
  silently did nothing, and `windows` / `chunks` / `dedup` on a map failed at
  runtime with an unbound name.
- Bind a shared-slice loop element by value, so `for n in xs` over a `&[T]`
  parameter reads the same as over the owned sequence.
- Write loop elements back through a `&mut` sequence parameter, and reach a
  heap element through the pointer its slot holds; the slot-address form read
  a Vec header out of the outer buffer and faulted in a native build.
- Type the element binding of a `&mut` loop whose element type is still being
  inferred, so mutating it no longer reports an immutable binding.
- Resolve a `use` that names a module in the same unit: `use options::Item`
  from the crate root, `use root::path::Item`, and nested `use a::b::Item`.
- Rebuild the tour of Gossamer: loops and loop expressions, every collection,
  the combinator surface, dates and times, HTTP serving and routing, HTTP
  clients and streaming, sorting, encoding, and tests - each step runs in the
  browser playground.
- Make the existing tuple type discoverable and complete its surface: a
  language reference page, a worked example, `%info Tuple`, and `%explain`
  element listings for tuple bindings.
- Fold `len()` on a tuple from its type. The dispatch fell through to the
  generic vec-header read, so a 3-element tuple reported 1 in a native build
  and 3 on the VM.
- Reject `iter()` and its combinators on a tuple: its elements may differ in
  type, so there is no element type to yield. `iter()` faulted in a native
  build and `count()` answered 0.
- Swap the sequence literal spellings: `#[a, b]` builds a `Vec<T>` and
  `[a, b]` builds a fixed `[T; N]` array. The repeat form follows the array
  spelling, so `[value; count]` is a fixed array and `#[value; count]` is
  `GP0033`.
- Parse chained tuple indices so `t.0.1` reads through a nested tuple.
- Assign tuple elements positionally on the bytecode VM, matching the compiled
  tiers: `t.0 = v` and `t.0.1 = v` now work everywhere.
- Render nested tuples in `{}` / `{:?}` on the compiled tiers through a
  self-describing tuple tag stream instead of failing to lower.
- Sort sequences of tuples structurally on the compiled tiers; the slot-wise
  integer sort reordered flattened slots rather than whole elements.
- Give every container exactly one name. `HashMap`, `HashSet`, `VecDeque`,
  `VecQueue`, `VecStack`, `BinaryHeap`, `MaxBinaryHeap`, and `MinBinaryHeap`
  are rejected with `GR0006` and a rename hint to `Map`, `Set`, `Deque`,
  `Queue`, `Stack`, `MaxHeap`, and `MinHeap`.
- Free the bare `Map` name for `std::collections::Map`; the concurrent map is
  reachable only as `sync::Map`, which fixes a native-tier crash on
  `Map::new()`.
- Remove the `<[...]`, `[...]>`, `^[...]`, and `_[...]` literal forms. Build a
  `Queue`, `Stack`, `MaxHeap`, or `MinHeap` with `new()` or `from([...])`;
  the retired spellings report `GP0032` with the constructor to use.
- Reject the bare repeat literal `[value; count]` with `GP0033`. A repeat
  literal is a fixed array, `#[value; count]`.
- Bind a `for` loop's iterable once. A literal, an `iter()` / `enumerate()`
  chain over one, and an `Iterator<T>` binding each restarted at element zero
  or crashed the compiled tiers instead of walking the sequence.
- Assemble the same project compilation unit in the language server as in
  `gos check` / `gos run` / `gos build`, so a sibling module's items resolve in
  an editor instead of reading as unresolved names.
- Report a type diagnostic once per source position. A signature's types are
  converted in two checker passes, so editors stacked the same message on one
  span.
- Match Rust's `VecDeque` method surface by rejecting `push` and `pop` while
  keeping explicit front/back methods.
- Add restricted `Queue<i64>` and `Stack<i64>` collection types with narrow
  FIFO and LIFO method surfaces: `push`, `pop`, `peek`, `len`, `is_empty`,
  and `clear`.
- Add `Iterator<T>` to the prelude and tighten `%info` / `%explain` metadata
  for the phase 1 collection method surfaces.
- Fix REPL member completion for typed persistent bindings so collection values
  such as `Stack` and `Queue` complete their methods after `x.`.
- Make native CI's full LLVM dumps and preserved build directories opt-in so
  large general shards do not exhaust hosted runner storage before artifacts
  can upload.

## 0.42.2 - Native CI batching, memory and enumerate() fixes

- Build native CI test binaries once per shard, then run the compiled test
  executables directly with per-target logs and `--test-threads=1`.
- Bump workspace crates and lockfile package versions to 0.42.2.
- Compact typed byte-vector map storage for `HashMap<i64, Vec<u8>>` and
  `HashMap<String, Vec<u8>>`, with ownership handling for both moved Result
  payloads and loop-carried `to_vec()` temporaries.
- Elide deep clones of fresh Vec Result payloads in MIR so `payload.slice(...)`
  materializes one dynamic byte buffer instead of two before map insertion.
- Fix native `enumerate()` of collected map-entry tuples so it preserves every
  tuple slot instead of crashing or reading a truncated entry.
- Fix bytecode `iter().enumerate()` over concrete collections so it snapshots
  the iterator once instead of repeatedly yielding index zero.

## 0.42.1 - Playground, literal docs, and editor completion fixes

- Add a browser Playground entry point to the docs site and wire it into the
  homepage calls to action and tour navigation.
- Fix the browser runtime path that made homepage and tour examples fail with
  `runtime error: unreachable executed`.
- Document every current collection literal spelling in the skill card served
  by MCP and `gos skill-prompt`: `[]`, `#[]`, `{}`, `#{}`, `^[]`, `_[]`, and
  `<[]>`.
- Teach LSP method completion about current collection literal receivers,
  `BTreeSet`, `VecDeque` aliases, and heap aliases so editor suggestions match
  the parser and checker surface.
- Apply ranged LSP `didChange` edits against the current document instead of
  treating each edit fragment as a full file, fixing save-time formatting after
  deleting an explicit `fn main` wrapper around top-level entry code.

## 0.42.0 - Collections, REPL listings, and native iterator fixes

- Collection typing with the compiled runtime: `VecDeque`,
  `BinaryHeap`, `MaxHeap`, and `MinHeap` are `i64` shapes, while `BTreeMap`
  follows the typed map runtime.
- Add `BTreeMap::from`, BTreeMap discovery/docs parity with HashMap where the
  shared runtime supports it, a short-lived misspelled `VecDeque` alias, and
  `VecDeque::clear`.
- Tighten collection `::from` argument checking so incompatible map and set
  sources are rejected instead of silently defaulting.
- Remove pagination from REPL meta commands; `%bindings` now filters only
  binding names, `%declarations` filters only declaration names, and `%bindings`
  renders fixed-array and set literal spelling.
- Add queue, deque, stack, min-heap, and max-heap pattern docs plus a runnable
  collection-patterns example.
- Add `<[...]>` queue literals, `VecQueue` as a `VecDeque` alias, `^[...]`
  `MaxHeap<T>` literals, `_[...]` `MinHeap<T>` literals, and heap REPL help.
- Fix `%info` filtering so exact short-name hits still match full
  module-qualified declaration paths.
- Fill `%info`/`%explain` metadata for payload helper methods and Vec iterator
  receiver methods so documented signatures match callable methods.
- Make the perf workflow less flaky by shortening noisy soak samples, reducing
  process-run counts, keeping hard caps, and turning baseline ratio regressions
  into warnings with uploaded logs.
- Fix native iterator ownership and ABI lowering so recursive functions that
  accept `Iterator<i64>` no longer crash when collecting a `Vec` iterator.
- Restore fixed-array inference for constant repeat literals such as `[0; 6]`
  when they flow into `[T; N]` storage.
- Keep the JIT off bodies with oversized fixed aggregate locals while preserving
  bytecode execution for those functions.
- Require canonical `std::` roots for standard-library `use` paths, reject
  typo prefixes, allow full item imports such as `use std::iter::skip_while`,
  and add `take_while`/`skip_while` sequence methods.
- Add byte-oriented `strings::byte_len`, `strings::byte_at`,
  `strings::substring`, and Rust-like `path::components` and `path::prefixes`
  plus `path::unique_prefixes` for lower-allocation path processing.
- Support destructuring patterns in closure parameters across interpreted and
  native execution.
- Fix native lowering for method-form `enumerate`, `chunks`, tuple `.get`,
  aggregate `min_by_key`/`max_by_key`, and `println` callbacks used as function
  values.
- Speed up generated `HashMap<String, i64>` native/JIT paths by using typed
  string-key helpers and typed string cleanup.
- Fix CI parity gaps for zero-argument channels, bare map-pair iteration,
  Cranelift string cleanup intrinsics, pipe placeholder indexing, and native
  `Iterator<i64>::next`.
- Keep relative `self`/`super`/`crate` imports out of standard-library typo
  checks, restore Option LSP completions, and align native string-slice errors.
- Keep release-build object cache keys stable across CLI reinstalls while still
  invalidating on LLVM codegen and runtime ABI changes.
- Buffer native `println!` output across lines so line-heavy compiled CLIs do
  not perform one terminal write per row.

## 0.41.0 - Collection literals and native lowering fixes

- Add collection literals: `[a, b]` for `Vec`, `#[a, b]` for fixed arrays,
  `{key: value}` and `{}` for `HashMap`, and `#{a, b}` for `HashSet` or a
  typed `BTreeSet`.
- Make empty `{}` infer as an empty `HashMap`, including typed empty map
  lowering across interpreter and native builds.
- Fix native LLVM lowering for projected call destinations, aggregate writes,
  intrinsic result stores, validation handles, and multiword Vec swap/pop
  cases. This includes the `@is_halted` undefined-symbol build failure,
  non-empty `HashMap::from` and map literals, nested tuple formatting for
  maps and Vecs, and HashSet/BTreeSet display.
- Fix runtime string ownership checks so foreign C strings are never probed
  before their allocation, and keep `gos_rt_vec_get_ptr` from creating stale
  shared borrows under Miri.
- Restore the ABI registry sort invariant for the `gos_rt_btree_set_new` entry.
- Treat native lowering gaps as backend bugs instead of per-function fallback
  cases, with MIR validation, LLVM symbol auditing, and regression tests
  guarding the old lowering-gap contract. Raw intrinsic arity is now checked
  through one shared MIR catalogue before LLVM lowering, including weak
  reference upgrade payload extraction.
- Lower Rust binding function references through the generated binding symbols
  instead of rejecting binding functions used as values.
- Lower set literals, `HashSet::from`, and `BTreeSet::from` through native
  codegen for scalar and aggregate elements.
- Fix project-relative Rust binding discovery and binding cache locking for
  entry-file builds outside the project directory.
- Restrict REPL discovery so `%i` covers language and standard-library entries
  while `%e` covers user bindings and declarations.
- Update examples, docs, REPL discovery text, migration guides, and the Tour of
  Gossamer for the new collection literal syntax and current stdlib names.

## 0.40.0 - Coherent sequences, safer execution, and complete discovery

- Restore explicit script execution with `gos run [FILE] [ARGS]...` and reject
  `gos FILE`. `run` accepts source files regardless of filename extension,
  completes all current-directory filenames, and forwards every argument after
  the file, including `--`.
- Keep bare `gos` as the REPL and `gos -e STRING` as inline execution. Help,
  examples, MCP, tests, and internal runners now use the explicit `gos run`
  form.
- Preserve multiline shell-completion help for bash, fish, and zsh, and fix
  deterministic native test output names on Windows.
- Filter `gos run` and `gos build` source-file completion to `.gos` files plus
  directories across bash, fish, and zsh.
- Require a semicolon or newline between adjacent non-block statements.
  `println` remains an ordinary first-class function.
- Complete `%info`, `%explain`, LSP completion, and `gos explain` coverage for
  the current standard-library and diagnostic surfaces. Associated functions now
  render as `[associated function]`, and detailed entries include examples.
- Add a `Defined in:` field to detailed REPL discovery, leaving standard
  namespace entries blank, and suggest prelude type casing fixes such as
  `HashMap` for `Hashmap`.
- Restore useful `Vec::from` discovery, add `HashMap::from({})` and map-literal
  syntax such as `HashMap::from({"one": 1})`, and document collection
  constructors with clear signatures and examples.
- Replay REPL inputs that call user-defined `&mut self` methods, so persisted
  bindings reflect mutations across later expressions and `%bindings`.
- Tighten references into lexical views: reject escaping references, temporary
  backing, reference fields and containers, goroutine or channel transfer, and
  invalid alias rebinding with GT0052 or GT0053.
- Match reference-pattern mutability for scalars, reject aggregate reference
  patterns with GT0054, and add a comprehensive assignment and mutability matrix
  plus lexical mutable-alias checks.
- Preserve safe recursive reference cursors by tracking alias backing roots and
  declaration scopes, while rejecting rebinding to shorter-lived inner aliases.
- Match Rust-style checked integer overflow for debug VM, JIT, and native builds,
  while optimized release keeps width-correct wrapping for `+`, `-`, and `*`.
- Lower explicit `wrapping_add` and `wrapping_mul` to MIR wrapping ops instead
  of runtime calls. This restores automatic arena regions and `ast-rewrite`
  release performance and memory use.
- Format multiline match arms correctly around optional commas and line or block
  comments, and keep multiline generic parameters aligned.
- Make fallible collection operations return `Option` or `Result` instead of
  sentinel values. This covers Vec bounds operations, ordered containers,
  queues, stacks, deques, heaps, and synchronized vectors. `HashMap::insert`
  now returns the previous value.
- Separate arrays, slices, and Vec throughout parsing, typing, lowering,
  execution, discovery, LSP, MCP, docs, and tests. `[T; N]` is fixed-size,
  bare `[T]` is unsized, and `Vec<T>` is the only owned growable sequence.
- Keep Rust-like array-to-slice and Vec-to-slice call-scoped coercions, reject
  implicit owned array-to-Vec conversion, and require explicit `Vec::from` or
  `.into()` for growable storage.
- Give Array, Slice, and Vec distinct method catalogs. Resizing and capacity
  methods are Vec-only; mutable slices and arrays expose only valid in-place
  methods such as `fill`, `swap`, `sort`, and `reverse`.
- Fix native and VM sequence lowering for fixed-array and mutable-slice
  mutation, iteration, sorting, reversing, swapping, filling, and packed `u8`
  Vec construction.
- Fix packed `[u8; N].to_vec()` in LLVM release output and keep Cranelift
  scratch aggregates in frame slots.
- Add lazy `Iterator::step_by`, preserve iterator type provenance, enforce
  single-pass iterator consumption, and align Iterator discovery with the
  methods implemented across tiers.
- Migrate serde derives, reflection, standard-library wrappers, examples,
  feature tests, and generated docs to explicit Vec types where owned growable
  storage is required.
- Give by-value Vec parameters and Vec-containing aggregates independent
  storage across VM, Cranelift, and LLVM. Nested Vec children are cloned and
  retained recursively at binding, call, goroutine, channel, struct, tuple, and
  fixed-array boundaries.
- Reject unsupported inline aggregate publication through direct `go` calls and
  incomplete channel aggregate shapes with GT0055. Top-level Vec publication
  remains supported by cloning before publication.
- Mark published Vec contents, recursive RC nodes, strings, nested Vec values,
  aggregate children, and shared map Vec entries as shared before another
  goroutine or channel can observe them.
- Fix method-call lowering for local-rooted field and indexed receivers,
  borrowed user methods, indexed String borrows, fluent temporary receivers,
  array-to-slice method arguments, and projected Vec temporaries.
- Specialize generic impl methods from the concrete receiver borrow and keep JIT
  admission from promoting unsupported aggregate-boundary callees after
  inlining.
- Fix compiled ownership for recursive enums and structs rebuilt by whole-local
  assignment. Large linked-list construction and bounded traversal are correct
  in the VM, forced JIT, and optimized LLVM output.
- Fix Cranelift inline Option and Result field assignment, owned constructor
  binding, shallow managed-field copies from fresh function results, and the
  struct/Vec stress RSS regression.
- Lower simple Option/Result carrier construction and payload/discriminant
  reads inline in Cranelift, avoiding the Win64 `i128` runtime-call boundary
  that made the Windows `vm_jit` promotion test fail.
- Improve bytecode performance with direct payload enum constructors, unboxed
  integer parameters, direct global calls, typed String byte paths, and typed
  wrapping checksum accumulation.
- Replace the cycle collector's linear candidate deletion with an indexed root
  buffer and immediate reclamation for candidates that reach zero.
- Keep bytecode-only user iterator markers out of native promotion, and admit
  fixed-array helper callees through the native dependency ABI when their entry
  boundary is not marshalable.
- Fix `gos run FILE` argument forwarding.
- Make VM `Vec::with_capacity` report impossible allocation sizes with the same
  `capacity overflow` panic as native execution.
- Refresh README, SPEC, SKILL.md, generated docs, LLM catalogs, MCP and LSP
  descriptions, examples, benchmark instructions, and command descriptions for
  the 0.40.0 CLI, sequence, reference, REPL, and discovery behavior.
- Replace the nested fence in `SKILL.md` with portable inline fence spelling,
  and split validation into `quick-check.sh` and `full-check.sh`.

## 0.39.1 - Native Vec tail returns, error messages, CI testing

- Coerce fixed-array tail expressions to `Vec` when returned from a
  Vec-returning function, preventing compiled programs from treating stack
  array storage as a Vec header.
- Include source file, line, and column positions in bytecode runtime call
  stacks, including the call site and the expression that failed.
- Split each cross-platform native general test lane in two and run the
  platform-invariant Cranelift correctness corpus once on Linux. Cross-platform
  tier-parity shards continue to exercise generated code on every native OS.

## 0.39.0 - Direct scripts, REPL discovery, optimizations

- Run `gos FILE [ARGS...]` directly, including executable `#!` scripts; the
  legacy `gos` command is removed. Add `-c`/`--command` for inline code.
- Make `%i` and `%e` list callable signatures by default; `-d` adds descriptions.
- Restore `%i` module-member listings and 20-entry pagination, without a
  redundant next-page prompt on the final page.
- Reject `let &mut name = value` when `value` is not already a mutable reference,
  with guidance for borrowing or creating a mutable binding.
- Preserve `&mut self` writes through indexed `Vec` fields when the receiver's
  type is inferred at the call site.
- Avoid temporary HashSet intersection allocations during immediate iteration,
  and inline word-sized Vec field indexing in native hot helpers.
- Fix `usize` and `u64` iterator reductions in the VM, which previously
  skipped unsigned mapped values and could return zero.

## 0.38.8 - REPL inspection, native builds, error messages, optimizations/fixes

- Fix `usize` indexing through struct fields and computed expressions.
- Make `%info`/`%i <target>` fall back to public-symbol substring search, avoid
  duplicate module entries, keep blank `%i` to the catalog directory, and
  remove keyword documentation from `%i`.
- Move `%help` to the top of the command list, make pagination notices
  flush-left, and accept `--all` as an alias for `-a`.
- Ship and select the matching musl runtime archive so default Linux release
  builds remain fully static after Gossamer is installed.
- Eliminate redundant allocations and missing drops in native
  `integer.to_string().chars()` loops.
- Canonicalize native `Vec<u8>` values as packed bytes, fixing LLVM parity for
  encoding, crypto, HTTP sessions, hashing, and file I/O.
- Support reference and `name @ pattern` forms in interpreter `let` bindings,
  preserving exact pointee and destructured field types in REPL `%bindings`;
  `%info name` now shows a binding's type, capability, and available methods.
- Improve/tighten error message verbosity and clarity

## 0.38.7 - REPL standard-library discovery

- List all standard-library modules and parent namespaces in `%i std`, and let
  shortened namespace queries such as `%i database` resolve to `std::database`.

## 0.38.6 - Diagnostics, REPL inspection, and CI test partitioning

- Make type errors name both incompatible types and point to the authored
  expressions that produced them.
- Refine REPL discovery: `%info` searches language and standard-library
  documentation only, `%b` and `%d` own session state, `%find` is removed,
  and long listings page at 20 entries with `--page` or `-a`.
- Make `%i std` list top-level standard-library modules and support namespace
  paths such as `%i std::archive`.
- Native CI is partitioned by generated-code coverage; macOS releases use the
  explicit LLVM `opt -O3` and `llc -O3` pipeline.

## 0.38.5 - Cross-platform release profile correctness and REPL refinement

- Make native debug and release MIR pipelines genuinely distinct: debug keeps
  call boundaries and lightweight canonicalisation, while release performs
  whole-program inlining and full MIR optimisation.
- Scope the LLVM loop-idiom workaround to static-musl links, preserving the
  measured Linux optimization without disabling optimized memory idioms on
  macOS, Windows, or dynamic Linux builds.
- Make macOS and Windows linker optimization policy explicit and expose
  `gos build --explain-profile` for a machine-readable profile decision trace.
- Configure the runtime before copying process arguments so macOS and Windows
  receive allocator policy before their first runtime-owned allocation.
- Make REPL `%info` prefer session bindings and declarations, then catalog
  lookup; add `%clear-history` and suppress untyped runtime entries rather
  than displaying fabricated `...` signatures.
- Isolate REPL test history and run non-native CLI/REPL checks in Linux general
  CI, reserving the cross-platform native matrix for generated-code coverage.

## 0.38.4 - REPL signature catalog & native aggregate iterator fix

- Fix native iterator mapping over struct values, which could segfault in a
  freshly built executable.
- Give every String method shown by the REPL info command a complete parameter 
  and return-type signature.
- Source those method signatures from the checker-owned standard strings catalog
  where possible.
- Native aggregate iterator mapping now uses a pointer-element callback ABI 
  instead of treating struct values as i64

## 0.38.3 - REPL banner, %help output, dependency cleanup

- Show the version and runtime architecture in the REPL banner.
- Add history navigation to the `%help` output.
- Remove the unused `gossamer-ast` dependency from `gossamer-binding`.

## 0.38.2 - Native lowering and REPL refinement

- Lower integer `abs()` calls through `gos_rt_math_abs_i64` in both LLVM call
  lowering paths, preventing malformed `llvm.fabs.f64` IR in native builds.
- Permit trailing line comments in REPL expressions, `let` bindings, and
  mutation classification.
- Replace `%ls` with `%info`/`%i`
- `%history`/`%h` now recalls and searches saved history.
- `%h` no longer is a shortcut for `%help`

## 0.38.1 - Native allocation, array construction, CI fixes, syntax fixes

- Build runtime-sized repeated arrays directly in their destination binding,
  removing a full deep clone and duplicate backing allocation from native
  array workloads.
- Batch native allocator page purges by default instead of issuing
  `madvise` for nearly every short-lived allocation. Immediate purging remains
  available with `GOS_ALLOC_PURGE_DELAY=0`.
- Keep the Windows native performance regression on platform-supported checks.
- Correct tuple-struct constructor diagnostics, optional match-arm commas,
  source-like integral float rendering in the REPL, and control flow for
  open-ended `for` ranges.
- Give REPL errors an accessible red treatment and move meta-command output
  to a muted, lighter palette distinct from source syntax highlighting.
- Keep generic hash-map insertion on the invariant-preserving dispatch path
  and avoid an intermediate allocation when formatting padded integer keys
  natively.
- Fuse padded integer key construction in VM and native execution, and reserve
  compact byte-map value arenas from actual payload width.
- Accept optional line-ending semicolons while continuing to remove them with
  `gos fmt`.
- Construct runtime-sized primitive repeated arrays at their final length,
  eliminating per-element checked pushes in native numeric workloads.
- Let REPL submissions combine persisted `let` bindings with following
  statements separated by either semicolons or newlines.
- Persist REPL `impl` blocks, complete methods on `HashSet` bindings, preserve
  struct elements during set iteration, and show set contents instead of
  internal handles in `%bindings`; make `%ls`, completion, and docs expose the
  same set API, with mapping available only through `set.iter().map(...)`.

## 0.38.0 - Assignment, function fixes, REPL cleanup, performance fixes

- Reject attempts to assign to literals and patterns that might not match in
  plain `let` bindings, with direct diagnostics and guidance.
- Diagnose a missing return value when a function declares a non-unit return
  type.
- Use the `>>>` REPL prompt and clean stacked meta-command output.
- Let REPL meta-command output use the full terminal width, and automatically
  indent continuation lines inside open braces, parentheses, and brackets.
- Add contrasting title and description colors to REPL meta-command output,
  making long help, symbol, binding, declaration, and history lists easier to
  scan.
- Wrap `gos --help` and subcommand help to the detected terminal width.
- Remove numbering from `%bindings`, `%declarations`, and `%history` output.
- Treat open-ended ranges as lazy iterators at statement and loop-body
  boundaries, including persistent REPL bindings and `for i in start..` loops.
- Keep the REPL session alive after user and unwind panics, preserving earlier
  bindings for the next input.
- Allow empty named structs such as `struct Unit {}` to be constructed by
  their bare name.
- Allow semicolons only as separators between statements on the same authored
  line, with trailing and line-ending semicolons remaining errors.
- Persist every binding from semicolon-separated `let` statements entered on
  one REPL line.
- Preserve packed VM storage for fixed arrays of all-`f64` structs when their
  elements come from constructor helpers or other expressions, restoring
  n-body performance after named struct initializers replaced positional
  literals.
- Extend Unicode character-position indexes incrementally when appending to
  VM and native strings instead of rescanning the entire accumulated string,
  restoring linear JSON builders and improving byte-oriented native workloads.
- Keep dynamic native `String.len()` on its O(1) Unicode-length index, restoring
  linear JSON rendering and checksum passes.
- Treat three-way vector swaps as ownership moves.

## 0.37.0 - REPL overhaul, Syntax changes (commas vs newlines), Fixes for Strings, Iterators, Ranges

- Make the REPL quiet by default, add command-wide `-v` plumbing, use the
  `gos>` prompt, print expression values without numbered markers, and wrap
  help and listing output to the terminal width with an 80-column cap.
- Reject trailing statement semicolons and consistently separate single-line
  brace lists with commas and multiline lists with newlines; normalize accepted
  multiline trailing commas with `gos fmt`.
- Distinguish unit, tuple, and named struct construction, and reject positional
  fields for named structs.
- Iterate bare strings as Unicode characters, start `..end` loops at zero,
  correctly consume ranges stored in bindings, preserve mutable vector
  bindings after REPL loops, and verify the Unicode regex contract across
  execution tiers.
- Allow heterogeneous tuple bindings in `for` loops, prevent malformed
  declaration lists from stalling parser recovery, and update perf benchmarks
  to named struct initializers.
- Keep MCP tests independent of binaries excluded from their CI shard.

## 0.36.2 - Nested functions and structs

- Support spec-compliant block-local function and struct items across VM and
  native backends, including recursion and lexical name isolation.

## 0.36.1 - Collection iteration and REPL fixes

- Make bare `HashMap` and `BTreeMap` values directly iterable as key-value
  pairs, quote string map keys in display output, and return unit after normal
  `for`-loop exhaustion.
- Verify direct iteration handling for vectors, arrays, slices, tuples, and
  hash sets.
- Accept attributed REPL declarations such as `#[derive(PartialEq)] struct
  Point { ... }`, and run derive synthesis before evaluating later inputs.

## 0.36.0 - Compact collections and trustworthy types

- Pack narrow scalar collections in the VM and compact native vector and
  byte-valued map storage without changing collection semantics.
- Store native fixed byte arrays at their declared element width while
  preserving fixed-array value semantics.
- Preserve established numeric binding types across later assignments, calls,
  returns, indexing, and collection operations.
- Correct consuming collection ownership so moved strings and vectors are
  reclaimed without invalidating live aliases.
- Give byte buffers and builders concrete public types, strict method contracts,
  packed-value interoperability, and private runtime representations.
- Fix tuple literal `for` loops and `&mut Vec<T>` loop element mutation across
  REPL, VM, JIT, and native execution.
- Reject malformed REPL `let` inputs without creating phantom bindings, and
  preserve byte-vector mutation and length semantics on the VM.
- Restore sequence `.iter()` and mixed-width integer length comparisons.
- Expand type-guarantee coverage across mutable, immutable, constant, nominal,
  nested, enum, collection, and standard-library handle values.
- Index and slice strings by Unicode scalar position without lossy UTF-8
  repair; use `as_bytes()`, `bytes()`, or `byte_at()` for byte access.
- Persist HashMap entry mutations in the REPL and reject assignments that
  replace a map with an `or_insert` result.
- Reliably print arithmetic expression results in interactive REPL sessions.

## 0.35.0 - Collection semantics and public diagnostics

- Show public, fully inferred types in diagnostics and REPL bindings without
  exposing internal inference names, report expected and supplied types in the
  correct order, accept indented REPL meta commands, and keep method help
  synchronized with the supported `String` surface.
- Support clamped and open-ended range indexing consistently on `String`,
  `Vec<T>`, `[T]`, and fixed `[T; N]` values across all execution tiers.
- Distinguish fixed arrays from growable vectors, reject Vec-only mutations on
  `[T; N]`, and support explicit conversion with `Vec::from([value; N])` or
  `Vec::from([a, b, ...])`.
- Preserve concrete types for empty collection constructors and enforce
  contextual integer-width bounds within collection literals.
- Persist `String`, vector, map, set, and deque mutations across REPL inputs,
  with collection-specific mutability, arity, argument, and return checks.
- Align collection methods with Rust where supported, including equivalent
  method and type-checked qualified collection calls, unit-returning inserts,
  element-returning `Vec::remove`, boolean `HashSet::insert` and
  `HashSet::remove` results, and in-place growth for uniquely owned `String`
  and `Vec` storage.

## 0.34.3 - Intcode native parity

- Support real `const` items in nested block scopes across VM, Cranelift, and
  LLVM lowering, keyed by lexical definition rather than stored as locals.
- Fix native lowering for `match` binding arms such as `op => ...`, so the
  binding receives the matched value instead of the binding name string.
- Fix additional native panics by cloning vector let-copies before mutating a
  reused input program.
- Add an Intcode regression covering interpreter, debug native, and release
  native parity for mutable-slice opcode execution and Part 2-style searches.

## 0.34.2 - macOS native Intcode parity

- Pin macOS LLVM object triples to the effective deployment target so native
  objects and linker settings agree.
- Emit an explicit aarch64 data layout matching Gossamer's 8-byte slot model
  and keep packed Result/Option stores at 8-byte aggregate alignment.
- Add an Intcode-style native parity regression for vector clone, indexed
  mutation, and opcode loops.

## 0.34.1 - Stream read_line match parity

- Make `io::stdin().read_line()` return `Option<String>` consistently across
  interpreter and native tiers, including EOF handling and trailing line-ending
  trimming.
- Preserve `read_line(&mut String)` as the buffer-appending
  `Result<i64, errors::Error>` overload.
- Tag `io::Reader` and `io::Writer` as opaque stream handles so match lowering
  sees the correct enum shape, and gate stream method fallback to stream
  receivers.

## 0.34.0 - Explicit standard library imports, filesystem fixes, &mut fixes, MCP & CI improvements

- Require `use` for every standard library module, including qualified calls
  such as `env::args()` and `fs::read(...)`, while retaining the documented
  primitive, collection, macro, concurrency, and helper prelude.
- Remove generated derive code's hidden `strconv` import dependency.
- Make resolver and LSP diagnostics, completions, import edits, and unused-import
  analysis accurate for missing, explicit, grouped, aliased, and top-level uses;
  suppress duplicate edits for modules already imported through a group.
- Make lexer, parser, resolver, type, and REPL errors more actionable with
  offending values, accepted forms, concrete fixes, and relevant constraints.
- Traverse top-level statements in shared immutable and mutable AST visitors.
- Require explicit `&mut` call arguments for mutable-reference parameters
  across functions, closures, qualified methods, pipelines, and goroutines;
  add a focused diagnostic and remove hidden VM and native auto-borrowing.
- Expand mutability regression coverage and clarify mutable bindings,
  reference forwarding, returned aliases, obvious overlapping mutable aliases,
  and concurrency boundaries.
- Preserve arbitrary OS names and paths returned by `fs::read_dir` across
  interpreter and native filesystem traversal.
- Refresh examples, conformance and REPL fixtures, specifications, generated
  catalogs, and stdlib documentation for explicit imports and opaque paths.
- Improve MCP guidance to require imports and prefer receiver methods or metadata
  already returned by APIs.
- Run independent Linux CI suites and release builds concurrently, and remove
  the redundant default-feature Clippy pass.
- Name mutable call arguments in diagnostics and show the required `let mut`
  and `&mut` forms directly in REPL output.
- Verify explicit mutable references across VM, Cranelift, and strict LLVM
  tiers, and keep raw-byte filename fixtures on supporting platforms.
- Split native tier-parity CI into independent backend-group shards.
- Link diagnostics to published documentation instead of repository paths.

## 0.33.6 - Methods and docs audit + fixes and mutability fixes

- Fix `String::parse` generic syntax and type inference, including
  `s.parse<i64>()`, `s.parse::<i64>()`, and expected `Result` payloads.
- Reject unconstrained parse payloads and keep missing `?` diagnostics specific
  to the inferred `Result` payload.
- Reject invalid `?` operands before lowering so non-`Result` and non-`Option`
  values cannot reach runtime-only match failures.
- Reject `Option`-only methods on `Result` values, including the Rust-like
  `String::parse` result surface.
- Align native `crypto::rand::bytes` with its `Result<Vec<u8>, errors::Error>`
  contract, including negative-count errors.
- Align `io::ReadAll`, `flag::Set::parse`, and `http::Client` request chains
  with their fallible `Result` contracts across checking, VM, and native tiers.
- Keep native `http::Request` handler params as opaque runtime pointers while
  preserving typed request field access in the checker and generated docs.
- Add `std::strings::parse` and document it on the strings stdlib page.
- Audit core type method discovery so REPL listings show user-facing method
  descriptions for String, Vec, map, set, deque, Option, and Result surfaces.
- Stop unused-mut linting from warning when mutable bindings are required for
  indexed or field-place writes.
- Preserve VM aliasing when a single mutable-reference argument is returned and
  bound locally, so writes through that returned reference update the original.
- Correct BTreeMap docs to stop advertising unsupported `remove`.
- Fold the standalone core method contract page into the standard library
  method support reference and generated stdlib entry points.

## 0.33.5 - REPL documentation metadata

- Expose runtime receiver methods in REPL `%help`, `%ls`, and `%find`,
  including `String::parse`, collections, `Option`, `Result`, iterators, and
  handle types.
- Add metadata coverage for every registered builtin `Type::method` surface.
- Correct `std::strings` conversion signatures and refresh generated stdlib
  docs/catalog output.

## 0.33.4 - VM recursion throughput recovery

- Restore bytecode VM throughput for shallow named function calls by using a
  bounded direct-call fast path before falling back to heap-backed explicit
  frames for deep recursion.
- Keep closure calls on the explicit-frame path and use a smaller debug-build
  direct-call limit so stack-safety regression coverage remains intact.

## 0.33.3 - VM regression fixes

- Recover bytecode VM recursive enum throughput by compiling nullary variants
  to their canonical values and keeping arity 1 to 2 payloads inline.
- Reduce bytecode VM numeric-loop dispatch by eliminating dead post-FMA float
  moves and fusing integer-to-float divisors into typed float division.
- Restore the no-temporary-vector path for two-integer struct construction
  while preserving shared field-name layouts.
- Refresh the generated stdlib API catalog and remove an obsolete `cargo-deny`
  duplicate-skip entry.

## 0.33.2 - JIT efficiency and updates

- Add lifecycle RSS attribution and native-code byte reporting around JIT
  promotion so compilation peak, post-finalization, and steady execution
  memory are measured separately.
- Compact finalized JIT metadata by replacing duplicate name and lookup maps
  with dense entry/signature tables and dropping compile-only state before
  artifact installation.
- Upgrade Cranelift to 0.134.2 and detach finalized native allocations from
  compiler module state, keeping only runtime-owned mappings and compact
  dispatch metadata alive during native execution.
- Share struct field-name layouts across instances to reduce repeated boxed
  object metadata while preserving declaration-order field access.
- Reduce VM memory by avoiding duplicate frontend source ownership, using a
  lightweight common `gos` path, compacting bytecode names, and preserving
  typed integer storage across container operations.

## 0.33.1 - Cache clearing, AOT performance recovery, and leaner JIT preparation

- Add `gos cache --clear` to remove every known Gossamer cache class without
  removing project build outputs or vendored dependencies.
- Recover native edit-distance throughput by limiting release loop-versioning
  clones that bloat dense dynamic-programming loops; retain aggressive
  versioning for JIT promotion.
- Reduce baseline JIT memory when no body can promote by discarding retained
  preparation state, and admit lowerable internal arrays, vectors, tuples,
  structs, recursive enums, and by-value results to Cranelift JIT.
- Add `GOS_JIT_TRACE` rejection diagnostics for unsupported promotion shapes.
- Parse the generated CLI schema on an explicitly sized stack so Windows-hosted
  cross builds do not overflow before command dispatch.

## 0.33.0 - Adaptive JIT coverage + improvements for civil time, filesystem libraries and caching

- Extend JIT promotion and native lowering for loops, recursion, native-only
  helpers, typed vectors, and header-backed string lengths while preserving VM,
  JIT, and LLVM AOT parity.
- Add immutable paths and civil time with signed pre-epoch precision, explicit
  DST gap and fold resolution, fixed offsets, and IANA locations across tiers.
- Extend `gos test` with listing, exact selection, ignores, skips, parameter
  cases, deterministic shuffling, fail-fast, process timeouts, and JUnit output.
- Add deterministic filesystem workflows and reusable structured command
  parsing with validation, subcommands, environment fallback, and completions.
- Bound Rust-binding runner caches, disable generated debug and incremental
  bloat, streamline CI, and align shared dependency versions.
- Make explicit `&mut` call arguments consistently write through, including
  fixed arrays.
- Restore release benchmark throughput by using host `-mcpu=native` for native
  builds and extending loop bounds versioning to invariant indices.

## 0.32.3 - LSP diagnostics and quick fixes, type info, and mutable-reference fixes

- Add default lint diagnostics, safe lint quick fixes, exact stdlib
  auto-imports, and `source.fixAll.gossamer`.
- Fix UTF-16 and Unicode position handling, percent-decoded workspace URIs,
  stale diagnostics after close, case-insensitive framing headers, and misplaced
  unused-variable fixes on mutable bindings.
- Show resolved nested reference types in REPL errors and LSP hover instead of
  leaking inference variables.
- Show each binding's resolved type in REPL `%bindings` output.
- Clarify mutable-reference alias chains and reserve documentation lifecycle
  labels for experimental features.

## 0.32.2 - Strict mutability, REPL shortcuts, REPL find regex

- Reject writes or mutable method calls when implicit dereferencing crosses
  any shared reference layer, including nested `&mut &T` escapes.
- Enforce immutable bindings for user and trait `&mut self` calls, implicit
  pattern bindings, and built-in collection and string writeback methods.
- Add `%h`, `%q`, `%b`, `%d`, `%l`, `%r`, and `%f` aliases for the primary
  REPL meta-commands and show them in `%help`.
- Keep Ctrl+C in the REPL as `KeyboardInterrupt`; only Ctrl+D, `%q`, and
  `%quit` exit.
- Make `%find` a regex path search and add optional regex filters to
  `%bindings` and `%declarations`.

## 0.32.1 - Strict function type checking, gos cache readability

- Enforce nominal and structural parameter, return, method, callback,
  pipeline, generic-call, and enum-constructor types, including mismatches
  whose numeric literal types resolve after the call is checked.
- Show `gos cache` sizes in compact, human-readable units by default.

## 0.32.0 - Struct construction overhaul and correctness fixes

- Breaking: named structs now require braced construction. Named struct
  literals accept keyed fields, positional declaration-order values, or a mix of
  both, while tuple structs use tuple declarations and parenthesized
  construction. Displayed struct values render as round-trippable source
  syntax.
- Restrict REPL `%find` fuzzy matching to symbol names so item descriptions
  do not make unrelated results rank as matches.
- Rebind REPL reference aliases to new temporary referents without mutating the
  previous immutable or mutable named referent.
- Fix reported REPL iterator and slicing issues: receiver-form `skip`,
  `enumerate`, and `zip`, pipe `..` range arguments, Vec range indexing,
  negative size/count arguments across collection, string, iterator, image, and
  runtime helpers, and bad `Vec::slice` arguments.

## 0.31.0 - Service hardening, REPL discovery, cache management

- Add configurable maximum request-header and chunked-trailer counts and
  sizes to the native HTTP/1 server. Trailer parsing is now bounded, requires
  valid CRLF framing, and rejects malformed header fields.
- Add `%find <query>` to the REPL. It fuzzily ranks public modules, functions,
  types, traits, constants, macros, and prelude builtins.
- Add `gos cache` inspection/pruning plus targeted `gos clean` cache classes,
  including Rust-binding runners that previously accumulated without a
  supported cleanup path.
- Add immutable structured diagnostic fields to `std::errors::Error`, for
  protocol and driver classifications such as SQLSTATE without message parsing.

## 0.30.2 - Native execution and memory efficiency, fixes

- Replace block-number loop detection in LLVM code generation with CFG
  dominance checks, eliminating cooperative preemption polls on ordinary
  backward control-flow joins.
- Charge compiled loop safepoints by estimated natural-loop work with a bounded
  16,384-unit budget. Native edit distance now runs at Go parity in the scaled
  benchmark while preserving cooperative scheduler polling.
- Add opt-in `GOS_PREEMPT_REMARKS`, `GOS_PREEMPT_STATS`, and
  `GOS_BOUNDS_REMARKS` diagnostics for loop polling and bounds fast paths.
- Elide checked vector access for non-negative queue indices advanced by one
  under a matching `index < vec.len()` guard, while retaining checks for
  larger or unprovable steps.
- Keep validated parsed JSON compact until its first value access, and render
  untouched documents directly without materializing a generic DOM.
- Bulk-copy proven uniform primitive rows during packed nested-vector
  conversion and report packed conversion rows and bytes in vec diagnostics.
- Rebind mutable reference aliases to arbitrary new referents instead of
  overwriting the previous referent, and reject overlapping named mutable
  borrows.
- Register lazy iterator methods on VM `Iterator` values and make open ranges
  start at zero, fixing issue-reported `take` and empty `..0` behavior.

## 0.30.1 - Correctness and CI reliability

- Prevented HTTP stress-test hangs with readiness polling, bounded retries,
  bounded shutdown, and a 120-minute workspace job limit.
- Fixed reference-alias liveness and exact fixed-array parameter
  copying across the VM, JIT, and native tiers.
- Made integer ranges lazy in every edition while preserving explicit integer
  bound types and Rust-compatible debug/release overflow for open upper bounds.
- Fixed lazy iterator method typing and dispatch across the VM, JIT, and native
  tiers without intercepting type-specific Vec methods.
- Allowed comma-free multiline match arms and made range, match-arm, pipe,
  alias, and type diagnostics more precise, with complete generated coverage.

## 0.30.0 - Lazy iterators, PKI, optimizations, and gos watch

### Language and editions

- Added manifest `edition = "2027"` while preserving 2026's eager iterator
  surface. Entry-file commands now read the edition from the project that owns
  the entry, not from the caller's current directory.
- Added edition-2027 lazy `std::iter` pipelines with `Iterator<T>` typing,
  range and Vec-backed sources, `map`/`filter`/`take`/`skip`/`enumerate`/
  `chain`/`zip` adapters, and consuming terminals including `collect`,
  `count`, `sum`, `product`, `min`, `max`, `fold`, `any`, `all`, and `find`.
- Added linear iterator diagnostics and runtime invalidation for borrowed Vec
  sources, preventing reused or formatted iterator state and detecting
  structural mutation during lazy iteration.
- Kept migration aliases permanently eager through `iter::eager_*`, and added
  all-tier fixtures proving 2026 eager behavior and 2027 lazy behavior across
  `gos`, forced Cranelift JIT, debug build, and release build.

### Native tiers and runtime

- Lowered typed iterator MIR through the VM, LLVM AOT, and Cranelift paths,
  including short-circuiting terminals, adapter panic propagation, cleanup of
  unconsumed iterator state, and nonescaping Cranelift range/take lowering.
- Wired the runtime ABI for lazy iterators, `String::with_capacity`, UUID v4/v7
  generation and UUID normalization, plus complete checked runtime symbol-table
  coverage for Cranelift JIT resolution.
- Improved compiled JSON rendering with direct reserved String writes and
  inline HTML-safe escaping, avoiding per-token ABI calls and the extra
  whole-document replacement buffer while preserving tier parity.
- Landed the structural-efficiency runtime pass: compiled `String.len()` is an
  O(1) header load, `String::with_capacity` is preserved through MIR and both
  native backends, packed primitive-row coverage is verified, VM backedge
  preemption is conditional, VM register spans are reused more aggressively,
  and VM thread stack reserve drops.
- Reduced JIT retained and transient state with SHA-256 artifact keys,
  move/filter of unique MIR/type snapshots into compilation, body-local
  slice-pattern rejection, default Option-local admission, and stable per-body
  promotion and rejection reports.
- Scoped lazy-Vec mutation overlays to live borrowed sources and reclaim them
  with the last iterator, preventing indexed workloads from accumulating
  per-element hash state; preserved `static mut` accessor call graphs so hot
  loops remain JIT-promotable.

### Security and standard library

- Promoted `std::crypto::x509` to Shipped with all-tier private-root server
  verification, mandatory CRLs, fail-closed handling for revoked, unknown,
  expired, malformed, wrong-host, and bad-chain inputs, and generated parity
  fixtures for VM, Cranelift JIT, and LLVM AOT.
- Added source-level private-CA peer-checking examples, a checked
  `gos bench` X.509 CRL workload, and security/docs updates that distinguish
  portable source verification from host TLS configuration constructors.
- Updated generated stdlib API and LLM reference metadata for 0.30.0.

### Tooling and build time

- Added `gos watch`, a restart-based development supervisor with direct
  frontend validation, highlighted status output, debouncing, port handoff,
  graceful HTTP shutdown, terminal clearing, lockfile enforcement, forwarded
  program args, and local path-dependency watching. `gos dev` remains a
  compatibility alias.
- Reworked `gos build` around a profile-aware native pipeline: debug uses the
  lightweight MIR path with minimal register promotion and `llc -O0`, while
  release uses the release MIR path and integrated Clang `-O3`.
- Amortized native loop-backedge preemption checks to one poll per 1,024
  iterations, retaining scheduler fairness without a runtime call per loop
  iteration.
- Added per-body and per-chunk LLVM object caching keyed by MIR, compiler,
  target, profile, PGO, debug-info, reproducibility, and LLVM tool identity,
  plus a final linked-artifact stamp cache for unchanged builds.
- Landed the structural-efficiency build pass: event-driven child waiting,
  cached Rust sysroot and LLVM tool discovery, one integrated Clang process per
  release codegen chunk, once-per-body MIR digests, lightweight debug MIR,
  removed no-comptime source cloning, worklist monomorphization, call-graph SCC
  codegen partitioning, no-op pipeline/link skips, and RAM-first LLVM job
  selection with `GOS_LLVM_JOBS` as the throughput override.
- Tightened native link selection and diagnostics across host, static-musl,
  Linux cross-target, and Windows MSVC builds, including target-specific
  runtime archive selection and verbose link-command tracing.
- Added `gos build --timings` JSON phase accounting for bundle, stamp,
  autoderive, comptime, frontend subphases, codegen, link, object counts,
  fallback use, parse-cache hits, and final-artifact cache hits.
- Corrected the build benchmark harness to isolate cold frontend caches, report
  dynamic and static release rows separately, repeat samples with median and
  spread, sample aggregate process-tree RSS, include cold/no-op/leaf-edit rows,
  and update the report parser, docs, and chart labels.

## 0.29.0 - Contracts, all-tier runtime, optimization, and standard-library depth

### Release contracts and packages

- Added distinct Stable and Shipped lifecycle states, item-level evidence and
  stdlib inventory, all-status export-drift checks, and an unfiltered release
  gate. Shipped modules no longer promote unlisted exports.
- Aligned the SPEC and user documentation with supported targets, tier
  behavior, package resolution, memory limits, iterator status, and release
  guarantees.
- Added `gos update`, source-aware dependency removal in `gos tidy`, and
  validated `project.toml` editions. Existing projects retain eager 2026
  iterator semantics; 2027 reserves the lazy signature migration.

### Language and iterator groundwork

- Added REPL `%help` entries for every built-in macro and the prelude
  assertion builtins, including format and pipe-placeholder guidance.
- Fixed mutable reference rebinding: a `let mut x = &value` binding retains
  its reference type and rejects assignment of a bare value.
- Enforced positional struct construction across the parser, formatter, REPL,
  VM, native tiers, fixtures, wrappers, and binary HTTP responses.
- Fixed pipe placeholders: `_` selects one direct argument or receiver;
  repeated and nested placeholders, and duplicate String slice receivers, now
  report precise errors.
- Corrected open-ended range patterns: `lo..` covers through the type maximum,
  while `lo..=` is rejected because an inclusive range requires an upper bound.
- Made all Rust-style format macros reject missing and unused positional
  arguments, require literal templates, and require an explicit `_` placeholder
  for piped values. Plain print functions retain their space-separated
  variadic behavior.
- Added internal linear `Iterator<T>` MIR state, source/adapter/next
  verification, Rust-hosted lazy range/slice/owned-Vec sources, adapters and
  terminals, plus callable `iter::eager_*` compatibility aliases.

### Scheduler, I/O, and observability

- Added amortized VM, Cranelift, and LLVM loop preemption and one-worker
  scheduler fairness coverage.
- Routed filesystem, process, stdin/stdout/stderr, terminal, TCP/Unix/TLS,
  HTTP/WebSocket, UDP, and SQL blocking operations through scheduler workers
  without holding registry or object locks; hardened the TSan UDP wake test.
- Added block and mutex wait profiles, scheduler Chrome traces, wasm-portable
  pprof behavior, and `runtime::cycle_collection_supported()`.

### Performance and build tooling

- Expanded `gos bench` with allocation, requested-byte, ARC, JIT tier-up,
  compile-time, code-size, RSS, and trampoline-copy telemetry, plus matched
  Gossamer/Go benchmarks and checked-in evidence.
- Reduced VM/JIT overhead through typed positional struct construction, raw
  heap-backed String and numeric storage, thread-confined write-back cells,
  flat-struct JIT sret, loop-entry tiering, scalar aggregate replacement, and
  build-phase source-map/RSS release.
- Added JIT code-budget controls and metrics, plus release PGO collection and
  profile options with validation and stale-profile warnings.

### Standard library, networking, and ecosystem

- Added OS, in-memory, subtree, and embedded-asset filesystems with
  deterministic walk/glob behavior and all-tier `fs::temp_dir`/`temp_file`.
- Added race-free `testing::TestServer` and all-tier `httptest::server`, HTTP
  diagnostics transport examples and benchmarks, cookie/transport test
  coverage, and fail-closed CRL-backed X.509/TLS verification.
- Added all-tier RGBA8 image handles with PNG/JPEG codecs, plus expanded Rust
  binding shapes including `Result<Bytes, String>` for ecosystem packages.

## 0.28.6 - Tuple structs, REPL declarations, and reference mutability

- Split some large low-risk source modules and tests.
- Added `%declarations` and tuple-struct constructors across the REPL, VM,
  and compiled tiers.
- `&mut` now requires a mutable source; writes through `&T` report a precise
  shared-reference diagnostic.

## 0.28.5 - String diagnostics, byte string literals, REPL bindings, char-patterns.

- Fixed `strings::count` and related string diagnostics so each parameter
  reports the expected type from the public signature, not a hard-coded
  `String | char`.
- Fixed byte string literal decoding so `b"..."` stores only the literal body
  bytes instead of including the `b` prefix and quote delimiters.
- Fixed REPL `%bindings` to show current values (`name = value` /
  `mut name = value`) and added tier-parity coverage for shadowed assignment.
- Changed string diagnostics to use canonical parameter names from `%help`,
  including `needle` for count/find/contains-style functions.
- Aligned char-pattern handling across checker signatures, interpreter
  builtins, docs, and generated API metadata.

## 0.28.4 - Iter stdlib surface

- Exposed `iter::collect`, `iter::once`, `iter::empty`, and free-function
  `iter::step_by` across check, interp, docs, and compiled lowering.
- Added `xs.collect()` runtime support to match the checker.
- Removed duplicate Rust-only iter helper spellings in favor of canonical
  public names: `chunk_by`, `windows`, and `chunks`.
- Fixed iter docs/signatures for data-last order and nested collection returns.

## 0.28.3 - Type errors, REPL display, and docs polish

- Fixed `strings::bytes()` typechecking and tightened stdlib string argument
  validation so wrong arguments produce one named error with the actual value.
- Aligned the `std::strings` method surface: receiver-shaped functions are
  accepted on `String`, while `join(parts, sep)` stays on `Vec`.
- Made REPL value display Python-like: bare strings and chars are quoted,
  while explicit `println!` output remains unquoted.
- Cleaned stdlib docs by removing redundant public-item tables, preserving
  `|` inside inline code, and improving table wrapping.
- Made docs repo header facts load uncached from GitHub instead of relying on
  stale session-cached source facts.

## 0.28.2 - Stability, correctness, and docs

- Made compiled callback teardown wait for in-flight calls before 
bindings may release their contexts.
- Hardened atomic stdlib writes against temp-file collisions and 
Unix rename durability loss.
- Docs - additional details & gaps filled.

## 0.28.1 - Compiled HTTP stability

- Restored compiled HTTP, TLS, and WebSocket keep-alive throughput by moving
accepted connections off the single global scheduler-poller lock. 
Connection admission remains bounded, sockets retain read/write deadlines,
and thread-admission failures return `503`.

## 0.28.0 - Correctness, optimization, and tooling

- Stopped rewriting frontend-cache entries on a cache hit; the validated AST
  blob now replaces the redundant fsync-backed success marker.
- Counted-loop HashMap construction now reserves a proven upper bound without
  changing map semantics; skipped-insert paths remain unoptimised.
- Added opt-in `GOS_VEC_ALLOC_STATS=1` telemetry for compact Vec inline,
  split-buffer, owner-carrier, and region allocation shapes.
- Primitive Vecs now keep generation and guarded metadata in the ABI tail,
  eliminating the separate owner allocation while preserving native prefix
  offsets; aggregate slot metadata remains lazy.
- Automatic loop regions now defer `runtime::collect_cycles()` until after
  `arena_pop`, keeping the collector away from region-owned pointers.
- Made counted-loop Vec reservation require exactly one push on every proven
  loop-body path, rejecting skipped, duplicate, cyclic, and exiting paths.
- Added generated LLM documentation drift checks to local `check.sh` and CI.
- Reject `HashMap::keys()` for aggregate key types before lowering; use
  `iter()` until typed aggregate-key snapshots are available across tiers.
- Document and test deferred-JIT admission: straight-line programs skip JIT
  MIR preparation, while loop and recursive candidates remain eligible.
- Expanded JIT accounting with compile duration, peak RSS, installed callable
  entries, and a conservative count of bytecode instructions bypassed.
- Added opt-in `GOS_PROFILE_RSS=1` phase samples for `gos` frontend, HIR,
  VM-load, and execution lifetime investigations.
- Propagated a local Vec bounds fact through repeated straight-line accesses
  after one proven guard, including empty single-predecessor bridge blocks,
  with conservative mutation/alias/join/branch bail-outs.
- Made scalar source indexing fail consistently across VM, runtime, and LLVM
  AOT paths instead of returning zero or silently dropping an out-of-range
  write.
- Made typed VM array fast paths require their flat storage representation;
  general-ABI array parameters now use the generic checked indexing path rather
  than silently falling back after an invariant mismatch.
- Routed `Vec::with_capacity` through active arena regions and added an
  all-tier bounded-RSS regression for returned structs containing Vec fields.
- Hardened package-download temp spools to be owner-only on Unix while
  streaming, hashing, and normal-Ed25519 signature-verifying archive bytes
  before validated extraction.
- Added typed native HashMap capacity construction for integer/string layouts,
  fixing `HashMap<String, _>::with_capacity` without selecting integer storage.
- Marked fully buffered HTTP/3 as Experimental in feature status.
- Clarified the generated LLM API catalog and interpreter cancellation parity.
- Made VM `String::with_capacity` reserve and retain mutable builder storage
  across String method write-back.

## 0.27.1 - %ls REPL command sorting
- `%ls` sorted alphabetically.

## 0.27.0 - Stabilization, optimization

- Strengthened release truth: stdlib modules are shipped catalog surface,
  generated docs and `gos feature-status` expose their status, and retained
  fuzz findings replay as named regressions.
- Hardened bytecode and VM execution with release-time validation, definite
  register initialization, explicit call frames, controlled RC exhaustion,
  and session-owned type/shape descriptor lifetimes.
- Renamed REPL `%dir` to `%ls` and expanded the shared stdlib catalog:
  `%help` shows documentation plus checker-owned full signatures, while `%ls`
  lists modules and their contents, never function members.
- Tightened HTTP servers: bounded admission, queues, framing, headers, bodies,
  deadlines, and graceful 503 backpressure; compiled HTTP/TLS/WebSocket
  connections use scheduler readiness and the interpreter admits normal
  256-connection keep-alive fanout.
- Hardened packages and credentials with redaction, structured URL/manifest
  parsing, transport limits, atomic cache/lockfile writes, archive/path limits,
  immutable git pins, and trusted-publisher verification.
- Hardened web auth with validated cookies, strong session/CSRF keys,
  authenticated expiry, and session-bound cookie CSRF tokens.
- Reduced VM and release overhead with runtime/std `opt-level=3`, typed
  struct-field access, direct typed-string scanning, compact large repeats,
  nested native JIT dispatch, and reclaimable type/shape compatibility handles.
- Reduced memory pressure by streaming source/cache hashing and JSON decoding,
  bounding package/cache trees, releasing JIT snapshots promptly, and fixing
  recursive-AST, JSON, and large-repeat VM RSS regressions.
- Added JIT RSS admission controls and metrics, plus time-and-RSS benchmark
  reporting and representative regression gates.
- Clarified stable versus Experimental surface and core namespace policy,
  split the specification accordingly, and added all-tier conformance and
  supported-target matrix checks.

## 0.26.0 - REPL help, stability, concurrency, performance, stdlib

### Performance

- **Cold-start frontend setup does less work.** Autoderive now skips its probe
  parse for sources with no lexical synthesis triggers, and CLI project
  manifest discovery is memoized for the current invocation.
- **Release Vec and parse hot paths are leaner.** The runtime now exposes
  byte-slice `strconv` parse helpers, real Vec reserve helpers, and a guarded
  MIR bounds-check rewrite for locally proven Vec indices.
- Release lowering now fuses parse-only `strings::slice` +
  `strconv::parse_i64` / `parse_f64` into range parse helpers, infers
  conservative counted-loop Vec capacity for fresh push-built vectors, avoids
  zeroing spare Vec capacity, reuses string length headers in append/concat
  fallbacks, and removes an extra allocation/copy from ASCII uppercase while
  preserving Unicode fallback behavior.
- Interpreted recursive enum transfer no longer retains dead native payload
  handles. VM-to-native enum construction now moves uniquely-owned recursive
  children into the parent where safe and releases retained/fresh fields with
  matching ownership on failure.
- **Recursive enum execution does less dispatch and synchronization.** Safe
  scalar-returning traversals can tier up through Cranelift; native consuming
  pattern extraction transfers uniquely owned children, and per-thread
  tag/shape caches avoid repeated global-table locks during node construction.
- The bytecode inliner now covers small straight-line helpers with local or
  `static mut` state updates, removing their repeated VM call frames.

### REPL and docs

- **The REPL now has searchable help and directory commands.** `%help`,
  `%help <symbol>`, `%help /regex/`, `%ls`, `%ls <namespace-or-symbol>`, and
  `%ls /regex/` expose stdlib modules, stdlib items, language feature status,
  and current command guidance without pretending to be Gossamer code.
- **Stdlib item metadata has one registry source.** Module/item docs, feature
  status, and REPL discovery now share sorted item records instead of parallel
  ad hoc tables.
- The new core method contract page records what must work across VM,
  JIT, and release tiers.

### Stdlib

- **Filesystem streaming handles are available across tiers.** `fs::File`
  and `fs::OpenOptions` now support open/create/read/read_to_string/write/flush
  and close in release and interpreted execution, with generated docs updated.
- **TCP streams gained timeout controls across tiers.** `set_read_timeout_ms`,
  `set_write_timeout_ms`, `clear_read_timeout`, and `clear_write_timeout`
  are wired for compiled and interpreted `net::TcpStream` handles.
- **Concurrency semantics now match Go more closely.** `channel()` /
  `channel(0)` are true unbuffered rendezvous channels, `channel(n)` is
  bounded, and `channel::unbounded()` is the explicit queue form. `select`
  readiness, context deadline wakeups, and `time::after` one-shot delivery were
  fixed across tiers.
- **Concurrency diagnostics and tests gained first-class hooks.**
  `runtime::scheduler_stats_json()` and `testing::wait_for_scheduler_idle()`
  expose scheduler state for deterministic tests and debugging.

### Stability

- **Safe recursive heap-enum producers tier up again.** Functions with only
  scalar inputs now build fresh enum trees natively; producers that accept
  RC-managed inputs remain on bytecode until their boundary ownership transfer
  is supported.
- **Cycle collection ignores tagged nullary enum values.** Graph walks no
  longer dereference a null payload after stripping a unit-variant tag.
- **Cranelift native cleanup now uses one summary-aware cleanup plan from block
  entry through return.** This removes a possible leak/double-free split when
  interprocedural capture summaries change drop placement.
- **Release and JIT channel typing is stricter.** `channel(n)` and
  `channel::unbounded()` now preserve the shared `Sender<T>` / `Receiver<T>`
  element type, and Cranelift preserves explicit channel capacity.
- **Runtime Vec stale-free handling is hardened.** Heap Vec headers are tracked
  while live so a repeated raw-pointer free returns before reading a reclaimed
  header, while the side table stays bounded by currently-live Vecs.
- **Runtime ABI drift checks now compare type classes, not just symbol names and
  arity.** The audit caught and fixed stale registry declarations for channel
  close, set/regex boolean-shaped returns, typed Vec element tags, and
  go-spawn function address words.
- **Forced-JIT parity now covers recursive enum/vector/string ownership
  fixtures.** The existing VM/native-boundary fixtures now run through the
  aggressive promotion gate so marshalling/freeing regressions are caught
  earlier.
- **Native CI is bounded and less failure-prone.** Cranelift native build tests
  now run the already-built `gos` binary with per-child timeouts instead of
  recursively invoking `cargo run`, the native CI job has an explicit timeout,
  and stdlib/ABI drift checks were updated for the new concurrency helpers.
- **Local validation output is easier to follow.** `check.sh` now prints quiet
  phase headers for the major gate groups before running the individual steps,
  so a local full check shows where it is without turning every command verbose.

## 0.25.1 - gos release optimizations

### Runtime memory

- **Allocator page purging now favors low release RSS.** Gossamer configures mimalloc with immediate page purging by default, while `GOS_ALLOC_PURGE_DELAY=<ms>` can restore batching for applications where fewer purge syscalls matter more than resident memory.
- **Auto-regions no longer reject scalar-only `static mut` callees.** The compiler still treats heap-owning static writes as region-unsafe, but scalar static updates such as deterministic PRNG seeds no longer block automatic loop regions.

## 0.25.0 - stdlib naming, runtime memory, conformance hardening

### Stdlib and docs

- **Stdlib names now use the canonical surface consistently.** Deprecated aliases were removed across interpreter dispatch, MIR lowering, type metadata, examples, and reference docs, with path, iterator, option/result, strconv, strings, sync, math, net, fs, env, and process examples updated to the new spellings.
- **Examples and generated docs were refreshed for the renamed APIs.** VM and native tiers now agree on the canonical stdlib examples, including filesystem directory metadata, string casing, vector reversal, sync primitives, math constants, and conversion helpers.

### Runtime memory

- **Interpreter strings and maps use denser storage.** Heap strings now use a thin refcounted backing allocation, and interpreter `HashMap` values use dense entry storage while preserving deterministic key-sorted user-facing iteration.
- **JIT startup and retained state were trimmed without dropping hot-loop promotion.** The in-process JIT lowers bodies serially, keeps first-call promotion for loop-bearing and recursive workloads, resolves promoted impl methods by chunk identity, and asks the allocator to collect transient compiler allocations after finalization.
- **JSON/string-heavy interpreter workloads stay on the native fast path where it is sound.** The JIT boundary now supports string-bearing struct receivers and `Result<String, errors::Error>` carriers, so recursive parsers and serializers promote without forcing unsafe enum-vector DOM builders into native code.
- **Allocator, lifetime, and stdlib lookup paths were tightened.** Mimalloc reclaims abandoned segments more aggressively while leaving page purging batched to avoid `madvise` storms on allocation-heavy workloads; stdlib resolver checks stay on static sorted tables, common read buffers shrink after loading, and statement-level last-use clears release large interpreter locals before later allocations raise the peak.
- **Aggregate tags are compact in the interpreter.** Struct and enum variant nodes now store integer type tags while still recovering the interned names for display, dispatch, equality, and native-tier interop.
- **Integer `math::abs` now matches compiled-tier semantics on the bytecode VM.** Integer inputs stay integer and use the saturating path; floating-point inputs retain floating-point behavior.

### Conformance hardening

- **Compiled-tier value handling was tightened across scalar carriers, aggregates, strings, and runtime shims.** Generic specialization now preserves scalar register classes, structural ordering is shared by sort/min/max, embedded-NUL strings keep their full contents, character joins use the correct layout, runtime errors own their messages, and dangerous overflow/OOB paths now diagnose or lower consistently.
- **`gos check` rejects more shapes that tiers cannot execute uniformly.** Oversized integer literals, primitive integer associated constants, SQL query-builder value arguments, and previously loose stdlib/method forms now resolve or fail before codegen instead of leaving VM/build divergence.
- **Cross-tier verification now exercises the real surfaces it names.** The parity harness runs the actual JIT tier, scans all runtime helper definitions, covers every feature-testing fixture, keeps stale skips out of the gate, and extends unsigned arithmetic/radix coverage.
- **Tier-parity fixtures now track canonical lowering paths.** Map-entry desugaring uses the public option surface, fixed-port router fixtures are covered serially, unsigned LLVM arithmetic follows declared types, and timing-sensitive channel examples avoid scheduler-dependent results.
- **Stdlib promises now match callable surface.** Non-callable manifest entries and stale guidance were removed or corrected, qualified encoding namespaces are wired consistently, signal notifier method forms build natively, and explicit LLVM fallback use is reported instead of hidden.
- **Latent lowering hazards were hardened.** Error vectors carry the correct deep-free tag, unknown struct fields no longer read as unit, and dynamic JSON field fallback is limited to real JSON values.
- **Large fixed arrays compile without source rewrites.** The compiled tier now spills large inline aggregate locals to runtime-managed storage, preserving fixed-array semantics without a type-checker stack-size rejection.

## 0.24.2 - static mut JIT lowering, parallel codegen scaling, fixes

### Performance

- **`static mut` compiles natively instead of declining to the VM.** A
  scalar `static mut` load or store now lowers to a native access on the
  Cranelift tier. Previously any `static mut` access kept its whole body
  on the bytecode VM, and one such access anywhere in a module held the
  entire module interpreted, so a hot loop sharing a module with a
  `static mut` counter never reached native code. Such programs now
  promote under `gos`, byte-identical across the VM, Cranelift JIT,
  and LLVM AOT. Every accessor of a static stays on one tier so the
  compiled cell and the interpreter's cell never diverge.
- **Parallel codegen fan-out scales to the host core count.** The LLVM
  backend's object-chunk fan-out follows `available_parallelism` rather
  than a fixed cap, while still holding enough bodies per chunk to
  preserve cross-body inlining.
- **Interpreter arithmetic dispatch inlines its hot path.** The adaptive
  integer and float arithmetic handlers fold into the bytecode dispatch
  loop.

### Fixes

- **REPL reassignments persist across inputs.** Assigning to a `let mut`
  binding from an earlier line (`name = "Mark"`) is now carried into later
  lines instead of being applied in a throwaway frame and discarded;
  compound assignments (`count += 1`) fold across lines in order.

## 0.24.1 - Iterator fusion, mutability enforcement, rust-bindings fixes

### Performance

- **Iterator pipeline fusion.** An `iter::` combinator chain over an
  integer range (`filter` / `map` stages feeding `sum` / `sum_by` /
  `count` / `product` / `product_by` / `fold` / `for_each` / `any` /
  `all`) compiles to a single loop with its closures inlined, matching a
  hand-written accumulator loop with no intermediate allocation, and
  identically across the bytecode VM, Cranelift JIT, and LLVM AOT.

### Correctness

- **Mutability is enforced.** Assigning to a `let` binding or parameter
  not declared `mut` is now a compile error (GT0030). Writes through a
  `&mut` reference and in-place mutating methods are unaffected.

### Language runtime

- **`gos --main-thread`.** Runs the VM on the process main thread so
  `[rust-bindings]` crates can call native libraries that require it
  (GLFW, OpenGL, Cocoa, Metal).

### Rust bindings

- `gos` / `gos build` no longer hang when a binding's `cargo build`
  emits more than a pipe buffer's worth of output.
- A `[rust-bindings]` call reached from a JIT- or AOT-compiled function
  no longer aborts the compiler.
- The generated binding build crate stands alone as its own Cargo
  workspace, so a project or cache directory nested inside another
  workspace builds instead of failing.

### Packaging and docs

- A statically linked `x86_64` musl `gos` binary is published alongside
  the glibc build.
- The language tour bundles CodeMirror as a local asset instead of
  loading it from a third-party CDN.
- New guide: building native binaries with zig.

## 0.24.0 - Performance, correctness, ergonomics (syntax, gos mcp)

A broad performance, correctness, and hardening release driven by a
benchmark audit against Go, Rust, C++, and the JVM/CLR languages.
Every change works identically across the three tiers
(bytecode VM, in-process Cranelift JIT, LLVM AOT) and is covered by a
tier-parity fixture.

### Compiled-tier performance

- **LLVM aliasing metadata.** The LLVM backend now emits TBAA metadata
  distinguishing `GosVec` header accesses from element-data accesses, so
  `-O3` hoists the loop-invariant data pointer and vectorizes element
  loops. Inline vector push also uses the statically known element stride
  instead of a runtime `elem_bytes` dispatch.
- **Single-allocation vectors.** A `Vec`/`[T]` now allocates its header
  and element buffer contiguously, removing one dependent cache miss per
  access, and reduces per-vector allocation overhead across the board.
- **Loop and string-building fusions.** `heap_u8_set` and a
  16-byte-stride tuple-vector get now inline on the terminator-call
  route; a `substring(i, i+k)` + `map.inc` pattern fuses into a single
  borrowed-key map probe with no per-probe string allocation; `dst +=
  format!(...)` fuses into direct append-formatting and the `Result`
  i128 carrier shims inline as pure bit operations.
- **`Vec::with_capacity`** is now available on all three tiers (it was
  previously missing on the bytecode VM).

### Interpreter performance

- **`gos` now promotes the implicit top-level `main`.** Hot loops
  living directly in `main` reach native code for the first time,
  unlocked by generalizing Cranelift's struct-return-via-out-pointer
  (sret) to every aggregate shape (struct, fixed array, 3+-tuple) and
  running the monomorphizer on the JIT MIR path.
- Idiomatic `[i64]` / `[f64]` slice-typed helper parameters, `Vec<f64>`
  arguments, and nested-vector returns now promote to native code
  correctly (they previously fell back to bytecode silently).
- **Interpreter memory.** Push-built homogeneous `[i64]` / `[f64]`
  vectors use a packed 8-byte-per-element representation instead of a
  boxed value array, cutting interpreter RSS on integer/float-vector
  workloads.
- **Fewer bytecode dispatches in loops.** Loop back-edges now re-enter
  the body directly (the fused increment-and-test already proved the
  bound, so the header check runs only on loop entry), and `for x in
  xs` iteration fuses its increment + bound test + jump into the same
  single dispatch the typed range loop uses. An i64 arith whose right
  operand is a small integer literal (`i % 7`, `n + 1`) executes as one
  immediate-operand instruction instead of a constant load plus arith,
  and an i64 local reassignment writes the arith result directly into
  the local's register instead of moving it there. Together these cut
  the per-iteration dispatch count of a typed accumulate-over-range body
  on the pure bytecode tier (`gos --no-jit`).

### Memory safety and correctness

- **RC leak fixes (deterministic reference counting).** Reassigning an
  owning container in a loop, a dynamic `[x; n]` repeat array, a
  returned string accumulator, and a `Vec`/`[T]` field of a by-value
  struct no longer leak on the compiled tiers. The Cranelift JIT now
  elaborates aggregate drops correctly, fixing a true unbounded leak in
  JIT-compiled hot loops.
- **Single-`Vec`/aggregate-field structs** (`struct Bag { items: [String] }`,
  `struct Sel { binds: [V] }`) now index, dispatch, copy, and drop
  correctly on every tier. The one-word "scalar single-slot" optimization
  was unsound for a sole field that is itself a `Vec`/`[T]` (indexing it
  read the buffer pointer as an element); such structs now use the normal
  address-backed representation and the `Ok(...)`-payload path retains the
  `Vec` field so it is not freed out from under the returned struct. Fixes
  an AOT SIGSEGV on `r.items[0].len()` and a JIT fault on a Vec-of-enum
  copy loop.
- **A heap enum passed by value into a closure** (`iter::map(|s| area(&s))`
  over a `[Shape]`) no longer reads its tag word as the value on the LLVM
  tier - the shape thunk forwards the handle pointer by value, so the
  closure stores it rather than dereferencing it.
- **`fs::walk_dir` / `list_dir` field access on the compiled tiers.**
  `for e in walk_dir(root)? { e.is_file / e.size / e.path }` now loads
  each `DirInfo` field through the blob pointer instead of reading the
  handle slot (or falling through to a JSON decode), fixing garbage
  output and a teardown crash on native builds.
- **A `switch` on a pointer discriminant** (a truthiness / null check on a
  heap handle) no longer emits invalid LLVM IR that fails the `opt` pass.
- **Returning `*s` from a `&mut String` parameter** no longer produces a
  use-after-free / SIGSEGV on the compiled tiers.
- **`Vec<enum>` and struct-with-enum-field comparison** now produce the
  correct answer on the AOT tier (previously silently wrong), and
  `vec == [array literal]` is fixed across tiers.
- **The cycle collector** now treats cross-goroutine shared objects as
  external live edges in every phase, uses a CAS claim for reclamation,
  and returns an owned reference from `Weak::upgrade`, closing a class of
  concurrent lost-update / double-free races.
- **Stack overflow** in AOT binaries and JIT-compiled recursion now
  raises a clean `GX0008` instead of a raw SIGSEGV (a main-thread musl
  stack-bounds bug was also fixed).

### Security

- **`json::parse` on malformed input** no longer SIGSEGVs on the
  compiled tier (a remote DoS for services parsing untrusted JSON).
- **Integer-overflow guards** on `Vec::with_capacity` / `[v; n]` size
  computation (heap out-of-bounds write) and a bounds + UTF-8 check on
  `HashMap.inc_at` (out-of-bounds read / info disclosure).
- **Git dependency URLs** are scheme-allowlisted, reject the `ext::`
  remote-helper transport and argv-injection, and run git with hardened
  protocol settings, closing a remote-code-execution path at
  dependency resolution.
- **Static file serving** now canonicalizes and confines paths to the
  document root (symlink-escape and absolute-path traversal fixed) with
  a size cap, on both the compiled and interpreter servers.
- **Request/response bounds**: the compiled HTTP/1.1 server caps request
  headers (431 on overflow), WebSocket continuation-frame reassembly is
  bounded, and the HTTP client caps the (post-decompression) response
  body against decompression bombs.

### Language and stdlib

- **Module-scoped function names.** Two modules may define the same
  function name: bare references inside a module bind to the module's
  own item, every reference lowers to the canonical `mod::name`
  spelling, and each tier defines the qualified symbol - previously a
  flat namespace made this a duplicate-definition error (GR0003).
  Type, const, and static names keep the cross-module uniqueness
  requirement.
- **Interactive child processes.** `process::spawn_piped(prog, args)`
  spawns a child with piped stdin/stdout and returns a `Child` handle
  with `write_stdin`, `close_stdin`, `read_line`, `read_stdout`,
  `wait`, and `kill`, on all three tiers - the JSON-RPC-over-stdio
  transport MCP clients need.
- **Path dependencies link at run, check, and build.** A
  `path = "../other"` entry in `[dependencies]` now inlines the
  dependency's source (transitively) into the compilation unit, so
  `use "project-id" as name` resolves to real code on every tier
  instead of faulting with GX0002 at runtime, and `gos check` rejects
  calls to dependency members that do not exist.
- **Option/Result chain methods**: `and_then`, `or_else`, `filter`,
  and `ok_or_else` now work in method form on every tier, and a
  combinator whose closure leaves a payload slot unresolved defaults
  it to the receiver's payload type so `{:?}` lowers natively.
- **`json::Value.set` on parsed objects.** The method form and the
  qualified `json::set` free call update parse-produced objects,
  accept scalar values (boxed to a JSON value on the compiled tiers),
  and chain, identically on every tier; `HashMap.set(..)` - which
  silently dropped the write - is now a `gos check` error pointing at
  `insert`.
- **Swapped-argument std combinators are check errors.** A data-last
  `option::*` / `result::*` free call whose trailing data argument is
  not Option/Result-shaped (the classic `option::and_then(opt, f)`
  argument swap, which silently returned `None`) is rejected with
  GT0029.
- **Unknown `json::Value` constructors and `process::Command` paths**
  are `gos check` errors instead of runtime GX0002 faults, and
  `sync::WaitGroup::new()` now resolves on the VM like its `sync::`
  siblings.
- **`gos main.gos` with a relative entry path** bundles sibling
  modules exactly like `gos .` (the module scan previously read
  an empty directory and silently bundled nothing).

- **Operator overloading** for the arithmetic, bitwise, index, and
  negation operators (`+ - * / % & | ^ << >> - []` and their
  compound-assign forms) via user `impl`s, on structs, enums, and
  generic structs, across all three tiers. Applying an operator to a
  type without the corresponding impl is now a clean `gos check` error
  instead of a runtime fault.
- **`use` groups accept multi-segment paths**: `use std::{env,
  encoding::json, strings}` now parses.
- **`std::iter` closure combinators over `f64`**: `iter::map`, `filter`,
  and `for_each` now pass `f64` elements through the float ABI on the
  compiled tiers (previously the element bits were handed to the closure
  in an integer register, yielding garbage), matching the bytecode VM.
- Fixed a compiler panic on `for (k, v) in pairs` when `pairs` was bound
  from an enum-variant payload, and made `x.downgrade()` on a non-`Arc`
  value a `gos check` error rather than a segfault.
- **Match-extracted enum payloads are fully typed.** A tuple-variant
  pattern's binders now carry the variant's declared payload types
  (borrows of them through a reference scrutinee; scalar payloads copy
  and bind by value), instead of inference variables that no later use
  pinned. Method-form combinators on an extracted payload
  (`Node::Call(xs) => xs.sum()`, `.map`, `.min`, ...) previously failed
  native builds with an undefined symbol or printed empty `{:?}`
  output; they now lower and format identically on every tier, and
  `gos check` catches payload type mismatches it silently accepted.
- **Vec payloads inside enums own their buffer.** An enum constructed
  with a `[T]` payload now retains a share of the vector and releases
  it when the enum is reclaimed, on every tier. Previously the
  constructing frame kept sole ownership, so an enum that escaped
  through a function boundary (a by-value argument returned by the
  callee) read a freed buffer on the compiled tiers - empty contents
  at best, a segfault under heap pressure - and a directly returned
  enum leaked its payload instead.

### Toolchain

- **Windows native HTTP handlers via router, middleware, and TLS.** On
  Win64 the rustc-compiled runtime reads a handler's packed
  `Result<Response, Error>` return from `xmm0`, while a gossamer
  `ret i128` returns it in the GP-register pair. Handlers registered
  through `http::serve` already crossed that boundary through a
  `<16 x i8>` return thunk; handlers registered through the `Router`
  verbs, `middleware` composition, `serve_tls`, and the HTTP/3 server
  now do too (previously they worked only when stale `xmm0` contents
  happened to match, and faulted otherwise). The registration shims
  that require the thunk are now a single audited table in the LLVM
  emitter.
- **`gos mcp`.** A Model Context Protocol server over stdio for AI coding
  agents: toolchain tools (`check` with structured diagnostics, `explain`,
  `run`, `build`, `test`, `fmt`, `doc`), semantic navigation (`hover`,
  `definition`, `references`, `workspace_symbols`) backed by the LSP
  analysis engine, and the skill card as an MCP resource and prompt.

## 0.23.1 - Playground parity: router, comptime, multi-program sessions

### `std::http::router` pattern lookup in the playground

The stateless router surface (`router::new` / `router::add` /
`router::lookup`, `Router::new` with its verb-method registration, and the
`Request::path_value` / `path_int` / `path_float` extractors) is pure
computation, so it is now linked into the wasm playground instead of being
gated out with the socket-bound HTTP stack. Route registration and lookup run
in the browser bit-identical to native `gos`; serving (`http::serve`)
remains unavailable on wasm. The router registry moved from the wasm-gated
websocket module into `http_router` itself so the module builds on every
target. The homepage's request-router example now runs in the playground.

### Comptime folds in the playground

The comptime evaluate-and-splice core moved from the CLI into
`gossamer-interp` (`fold_into_source`) and the playground runs it ahead of
its pipeline exactly as `gos` / `gos build` / `gos check` / `gos test`
do. `comptime { ... }` blocks, `comptime fn` calls, and `codegen!` splices
fold to the same literals in the browser as on every native tier.

### Goroutine worker reuse is keyed to the program

A pooled worker's cached VM is reused only for the program whose globals it
was built from, and rebuilt otherwise. A thread can outlive one program - the
wasm playground runs every goroutine on the main thread across successive
`run()` calls, and an embedding can load several programs in one process - so
goroutines spawned by a later program now always resolve callees against that
program's own globals instead of the first program's.

## 0.23.0 - Raspberry Pi target, limited cross-compilation, optimizations and fixes.

### Raspberry Pi (aarch64-linux) as a verified target

The bytecode VM (`gos`), the in-process Cranelift JIT, and native
compilation (`gos build`) are now exercised on 64-bit ARM Linux in CI, not just
cross-built and shipped. `gos` is fully self-contained on a Pi (in-process
JIT, no external tools); `gos build` uses the device's system LLVM (`llc`/`opt`)
and C compiler. The static-musl release link selects the musl sysroot by host
architecture rather than hard-coding x86-64.

### Limited cross-compilation

`gos build --target <triple>` produces a real, runnable native binary instead
of the previous "cross-link pending" stub.

- **Target-aware codegen.** A `--target` triple drives the LLVM `-mtriple`, the
  i128 ABI marshalling, and the incremental object-cache key through the single
  `host_triple()` chokepoint. `-mcpu`/`-mattr` derive from the resolved target
  arch (a generic ARMv8-A baseline for aarch64, never the host's `native` or the
  x86-only `+prefer-256-bit`).
- **Per-target runtime archives**, resolved by target with no host-archive
  fallback (a missing target archive is a clear error, never a foreign-arch
  mislink).
- **Host-agnostic linking.** Linux, macOS, and Windows hosts all cross-compile
  to every Linux target - `{x86_64,aarch64}-unknown-linux-{gnu,musl}`. The
  musl-static path is host-agnostic (rustup's self-contained CRT + `ld.lld` on
  every host); the gnu-dynamic path uses the matching `*-linux-gnu-gcc` on a
  Linux host and a `GOS_CROSS_SYSROOT` on macOS/Windows.
- **Validation.** Cross output is checked against the bytecode VM under QEMU in
  CI, across all three host OSes.

Cross-compiling *to* macOS or Windows as a target remains out of scope (needs
external SDKs).

### Compiled-tier optimizations

Narrowing the gap to Go on the stress-test microbenchmarks (`edit-distance` now
beats Go, `radix-sort` matches it).

- **Inlined scalar `min` / `max`.** `min(a, b)` / `max(a, b)` on `i64` lower to
  a branchless `icmp`+`select` instead of a `gos_rt_min_i64` call, removing a
  per-iteration FFI boundary from tight loops (a Levenshtein DP cell does two
  `min`s per step) and unblocking vectorization. Value-identical to the runtime
  on every tier.

- **No arena region around scalar-tuple loops.** A loop whose only heap
  "allocation" is a scalar tuple return (e.g. `let (n, r) = lcg(s)`) is no
  longer auto-wrapped in an arena region: a tuple of scalars is
  register/sret-returned and never hits the heap, so the region was two
  `arena_push`/`arena_pop` calls per iteration for nothing. The region
  eligibility analysis now counts a tuple as heap-allocating only when it
  carries a heap element.

- **`noalias` on fresh vec allocators.** `gos_rt_vec_new` /
  `gos_rt_vec_with_capacity` (and their typed variants) return a freshly
  `Box`-allocated header, so their returns are marked `noalias` (as `malloc`
  is), letting the optimizer prove a store through an unrelated pointer cannot
  clobber a vec header.

### Native enums: `Vec`-bearing variants

Step 8 (one native representation for user enums, shared across the bytecode VM
and the compiled tiers) now covers enums with `Vec<Enum>` / `Vec<(String,
Enum)>` variants - JSON-like `List` / `Map` shapes previously kept boxed. A
`Vec` element crossing the JIT boundary is built as a fresh, exclusively-owned
native copy rather than an alias of a live VM node, so construction and teardown
stay uniform (no mixed-ownership double free). Bit-identical across all three
tiers.

### Error-path fixes (tier parity)

- **Entry-point `Err` is reported, not dropped.** A `main` that returns
  `Err(e)` - an explicit `fn main() -> Result<..>` or the implicit
  `?`-desugared top-level main - now prints `e`'s Display (the colon-joined
  cause chain) to stderr and exits nonzero on every tier. Previously `gos`
  discarded the return value (silent, exit 0) while `gos build` exited 1 with no
  message - a tier divergence.

- **`fs::read_to_string` propagates errors on the native tier.** A missing or
  unreadable path now returns `Err` under `gos build`, matching `gos` and
  `fs::read`; it previously returned a silent `Ok("")` because the compiled-tier
  shim returned a bare string that could not express failure.

### Structural equality on heap enums

`==` / `!=` on a heap (recursive / `Box`, or `Vec`-bearing) enum with no user
`eq` impl now compares structurally on the compiled tiers: two distinct
allocations of an equal value are equal (`Tree::Leaf(1) == Tree::Leaf(1)` is
`true`), matching the VM's `values_equal` instead of comparing node pointers.
Driven by a per-enum descriptor and a runtime walk over scalar / `String` /
nested-enum / `Vec<Self>` fields; bit-identical across all three tiers.

### Derived JSON serde covers more field types

`#[derive]`-free `to_json` / `from_json` (and the `toml` / `yaml` pair) now
handle `Option<T>` (JSON `null` <-> `None`; a missing object key decodes to
`None`), tuples `(A, B, ...)` (JSON arrays), `HashMap<String, V>` (JSON objects,
keys sorted so the text is deterministic across tiers), and `json::Value`
(dynamic pass-through) - in addition to the previous scalars / `String` /
`Vec<T>` / nested structs, matching the documented surface. A struct field whose
type still is not serializable now produces a clear error (`GP0022`) naming the
field and its type when the struct is used in a serde call, instead of silently
dropping the whole struct's serde and surfacing only an opaque unknown-name
error at the call site.

### Auto-derived code sees structs in every bundled file

`to_json` / `from_json` (and the `toml` / `yaml` pair), the `#[derive]` /
structural `fmt` / `eq` / `cmp` synthesis, and `typeInfo::<T>()` reflection now
reach struct and `impl` declarations wherever they live in a multi-file
project, not only in the entry file. A package's sibling files are auto-bundled
by wrapping each in `mod <stem> { ... }`, so a type declared in `src/other.gos`
sits one module level deep in the merged source; the synthesis passes now
descend into inline module bodies - the same flattening the resolver already
applies for name resolution - so `from_json::<T>(...)`, a `#[derive]`, or
`typeInfo::<T>()` on a type declared in any bundled file resolves the
synthesized code on `gos check` / `gos` / `gos build`.

### `gos test` discovers tests in cross-referencing package files

`gos test` collected each file's `#[test]` names by fully resolving and
typechecking that file in isolation, which failed for any file whose top-level
code names a sibling module's item by bare name (the shared-root-module layout
of a multi-file package) - the file only typechecks against the bundled whole
package. Discovery now parses for `#[test]` names from the syntax alone, so
every test-bearing file is found; execution still bundles siblings exactly as
`gos` / `gos build` do. Previously a multi-file project would report "no
`#[test]` functions found" (and stream spurious `cannot find <sibling item>`
errors) whenever a test-bearing file referenced a sibling.

### Nested struct-field reads resolve against the leaf type (tier parity)

Reading a struct-typed field through a projection (`outer.inner.field`) now
resolves the leaf field against the inner struct's type by walking the pinned
MIR projection, rather than the root binding's type, so it lowers to a direct
slot walk on the Cranelift and LLVM tiers. A projected read whose receiver
carried only partial type info - an inference variable, or the `json::Value`
default the checker assigns an opaque nested field - is no longer routed through
the dynamic `json::get` path, so a struct-typed field of an `Ok(..)` / `Some(..)`
payload (the shape a derived `from_json` returns) reads back bit-identically
across the VM, Cranelift, and LLVM tiers. Covered by
`nested_struct_variant_payload.gos` in the tier-parity suite.

### CI

- musl cross builds (`cross-from-{linux,macos,windows}`, and `release.yml`'s
  aarch64 leg) now go through `cargo zigbuild` instead of a bare cross
  compiler, which had no musl sysroot on any host. Zig is installed via a
  plain `curl`/`tar` step rather than `mlugg/setup-zig` (stale tarball-name
  assumption plus a Node 20 runtime). `check.sh` gained a matching local
  gate.
- Fixed the cross-compilation bugs that surfaced once the above let the
  Linux/Windows QEMU jobs actually run: `qemu-aarch64` needs the real
  aarch64 runtime libs (installed via Ubuntu's arm64 multiarch archive,
  version-matched to the host's pockets); the Windows lld lookup now falls
  back to `rust-lld -flavor gnu` when the pre-named `gcc-ld/ld.lld` wrapper
  isn't shipped; cross-built Windows binaries no longer get a stray `.exe`
  (`platform_exe_name` was keying off the host OS instead of the target's);
  and the QEMU-diff script restores the executable bit that
  upload/download-artifact strips from every file.
- Fixed a broken rustdoc intra-doc link in `jit_call.rs`; `check.sh` now
  passes `--document-private-items` to `cargo doc` so this class of bug
  fails locally.
- Fixed a macOS test that assumed the host is never `aarch64-apple-darwin`
  (false since `macos-latest` went Apple Silicon in 2024).
- Bumped `actions/upload-artifact`/`download-artifact`/etc. off Node 20;
  `cross-from-macos` untaps the runner's pre-tapped `aws/tap` to silence a
  Homebrew warning.

## 0.22.0 - Comptime code generation, optimizations, core fixes, docs refresh

### Comptime code generation

Where 0.21.0's comptime folded to constant values (and `typeInfo::<T>()` could
build a constant *string*), 0.22.0 generates native *code* from reflection -
the reflection-driven codegen the Zig-style model is for.

- **`inline for` over `typeInfo::<T>()`.** A `for (name, ty) in
  typeInfo::<T>()` loop is unrolled per field at compile time, in the single
  compile (no fold pass): `name` / `ty` are comptime, `field_of(v, name)`
  projects the concrete field, and a `match` / `if` over the comptime field
  type folds to the taken arm. The emitted body is ordinary native field code -
  identical on every tier, no runtime reflection, no build-time tax.

- **Generic specialization.** A reflection-driven serializer is written once as
  `fn rec<T>(v: T) { ... typeInfo::<T>() ... }` and specialized per turbofish
  call site (`rec::<User>(x)`); concrete-type loops (`typeInfo::<Point>()`)
  need no turbofish.

- **`codegen!(...)`.** Splices a `comptime fn`'s `String` result back as raw
  source, for code generation beyond the `inline for` shape.

  Wired across the bytecode VM, Cranelift JIT, and LLVM AOT (new
  `comptime_inline_for.gos` and `comptime_codegen.gos` parity fixtures; LLVM
  lowers the emitted field code natively with no fallback).

### Transparent type aliases

`type X = T` is now transparent: `X` is interchangeable with `T` in let
bindings, parameters, returns, struct fields, composites, and alias chains, and
a generic alias `type Pair<A> = (A, A)` substitutes its use-site arguments.
(0.21.0 shipped aliases that parsed but failed every use with an opaque `adt#N`
type error.) A cyclic alias is rejected at check (`GT0024`). New
`type_alias_transparent.gos` parity fixture.

### Tuple structs

`struct Pt(i64, i64)` is now fully usable: construction (`Pt(3, 4)`), positional
access (`p.0`), destructuring (`let Pt(a, b) = p`, `match`, and function
parameters), `#[derive(Clone, PartialEq, Default, Debug)]` (Debug renders the
tuple form `Pt(3, 4)`), serde (`to_json`/`from_json`, a position-keyed JSON
object), and use in collections / as generic arguments. (Construction and
access were broken from 0.17.) Tuple fields are modelled as named fields
"0".."N-1", so everything lowers through the named-field path identically on
every tier. New `tuple_structs.gos` and `tuple_struct_serde.gos` fixtures.

### Structs and enums compare by value - no derive

Structs and enums are value types, so `==`, `!=`, `<`, `<=`, `>`, and `>=` now
work on them by value with **no `#[derive(...)]`** - exactly as they already did
on tuples. `eq` / `cmp` are synthesized automatically for every type whose
fields are all comparable (scalars, `String`, or nested comparable types);
ordering is lexicographic by declaration order for structs and by variant rank
then payload for enums. A user `impl` of `eq` / `cmp` overrides the synthesized
one (custom ordering). `#[derive(PartialEq/Eq/PartialOrd/Ord)]` still works to
force synthesis for generic or container-field types the automatic gate is
conservative about. Previously `struct == struct` without a derive faulted at
runtime and `struct < struct` never worked. New `structural_comparison.gos`
parity fixture.

Fixes a related lowering gap: `String <` / `<=` / `>` / `>=` now lower on the
LLVM backend (they fell back to Cranelift and were rejected under strict
lowering); and a `..` rest in a multi-field tuple variant (`E::C(..)`) now
matches (it only matched single-field variants before).

### Operator overloading: `%`, unary `-`, `[]`, bitwise, shifts

Operator overloading is extended from `+ - * /` to also cover `Rem` (`%`),
`Neg` (unary `-`), `Index` (`a[i]`), the bitwise operators (`| & ^`), and the
shifts (`<< >>`); each routes to the matching trait method (`rem`, `neg`,
`index`, `bitor`, ...) on a user struct / enum. Compound assignment continues to
route through the binary operator. New `operator_overloads.gos` parity fixture.

### Conversions: `x.into()` and `x.try_into()`

`x.into()` converts to the inferred target type `B` via its `B::from(x)` impl,
and `x.try_into()` to `Result<B, E>` via `B::try_from(x)` - the method forms of
the already-working `B::from(x)` / `B::try_from(x)`. The target is taken from
the use-site type (`let B`, a `B` parameter / return).

### Desugar macros: `matches!`, `todo!`, `unimplemented!`, `unreachable!`, `dbg!`

Five new macros expand at parse time into ordinary constructs (so they lower
uniformly on every tier): `matches!(expr, pat)` is a boolean `match`; `todo!` /
`unimplemented!` / `unreachable!` are `panic!` with a fixed (or supplied)
message; `dbg!(expr)` prints `expr` (Debug) to stderr and yields its value.

### Derives are limited to the ones that mean something (`GT0025`)

`#[derive(...)]` now rejects names that synthesize nothing - `Clone`, `Hash`,
`Copy`, `Display`, `Serialize`/`Deserialize`, and the conversion / operator
traits - with a hint pointing at the automatic behavior or at `impl Trait for
T`. The derivable set is `Debug`, `Default`, `PartialEq`, `Eq`, `PartialOrd`,
and `Ord`. `Clone` is rejected because structs are value types - `let b = a`
copies, and `a.clone()` is a universal builtin that works with no derive (as
are hashing, comparison, and serialization).

Also fixed: an inline same-variant comparison (`E::B(1) < E::B(2)`) now anchors
its operands to the enum so it dispatches; `value.downgrade()` / `weak.upgrade()`
resolve on a concretely-typed receiver; and an irrefutable enum let-destructure
(`let E::P(m, n) = e`) now reads its payload discriminant-aware on the native
tiers (it previously read the discriminant slot as the first field, yielding
garbage - `match` was already correct).

### Pattern destructuring in function parameters

A non-trivial parameter pattern - tuple `((a, b): (i64, i64))`, struct
`(P { x, y }: P)`, or tuple-struct `(Pt(a, b): Pt)` - now binds its components.
Previously only a single name per parameter was bound, so any destructuring
parameter faulted at runtime. New `param_destructure.gos` fixture.

### Collections

- **`BTreeMap` with i64 keys.** `BTreeMap<i64, i64>` (and `<i64, String>`) now
  works - backed by the same key-sorted `IntMap` machinery as `HashMap<i64, _>`,
  so insert / get / contains / len and key-sorted iteration behave correctly.
  (Previously only `<String, i64>` was supported; an i64-keyed insert faulted.)
  New `btreemap_i64_keys.gos` fixture.
- **`VecDeque` both ends.** Added `push_front`, `pop_back`, `peek_front`, and
  `peek_back` (only `push_back` / `pop_front` existed before), wired across the
  VM, Cranelift, and LLVM tiers. New `vecdeque_full.gos` fixture.

### Interpreter memory

Two interpreter-only optimizations cut peak memory on allocation-heavy tree
and record workloads; output and compiled-tier behavior are unchanged (tier
parity holds across all three tiers).

- **Small-variant interning.** A variant whose payload is a single small
  immutable scalar (`None`, `Some(0)`, an enum leaf like `Num(7)`) is shared
  from a per-thread cache instead of allocated fresh. The cache holds a weak
  reference, so every concurrently-live identical leaf of a tree shares one
  allocation while `downgrade()` / `upgrade()` liveness stays faithful.
- **Per-iteration loop release.** A loop body's own `Value` registers - the
  tree it just built, the temporary tuple a `let (a, b) = f()` destructure
  leaves behind - are released at the back-edge (new `ClearRegs` op) rather
  than lingering until the next iteration overwrites them, so consecutive
  build-then-rebuild iterations no longer overlap working sets.

Together they sharply reduce interpreted peak memory on tree-shaped
workloads, with no regression elsewhere.

### Interpreter speed: enum-heavy code now runs on the JIT

The in-process JIT marshals an enum value across the call boundary only in
the compiled `NativeEnum` representation, so an enum a bytecode function
built (a `Value::Variant`) used to fall back to the bytecode interpreter on
*every* call - leaving enum-recursive programs (tree walks, recursive
rewriters, structured-data transforms) entirely on the slow tier, and even
paying a wasted marshal attempt per call. Three changes fix this; output
stays bit-identical across the VM, JIT, and AOT tiers.

- **`Value::Variant` enums marshal into the native representation** at the
  call boundary (mirroring how strings / vecs already cross), so an
  enum-argument function runs as native code instead of falling back. The
  temporary is reclaimed after the call.
- **Enum-*returning* functions too**, gated by a sound interprocedural
  "returns-fresh" analysis: a body whose result provably originates from a
  fresh allocation (a constructor, or a call to another fresh body) rather
  than a passthrough of its input is safe to marshal-and-free. A
  tree-rebuilding transform qualifies; an `unwrap`-style passthrough does not.
- **Demote-on-repeated-fallback**: a body whose arguments never marshal is
  returned to bytecode-only after a short streak of misses, so the JIT stops
  taxing it.

Net: enum-recursive interpreted code that previously ran entirely on the
bytecode tier now executes as native code.

### Interpreter speed: `byte_at` on a string is a direct op

`s.byte_at(i)` on a statically-`String` receiver now lowers to a dedicated
bytecode instruction instead of a general method call, skipping the
argument-materialisation, inline-cache probe, and builtin dispatch that a
method call carries. Byte-scanning loops - hand-written lexers, parsers, and
UTF-8 walks that index a string a byte at a time - run noticeably faster in
the interpreter. The fast path applies only when the receiver's type is known
to be `String`, so a user type with its own `byte_at` keeps its method; the
result is identical on every tier.

### Interpreter speed: more value shapes reach the JIT

The in-process JIT marshals more value shapes across the call boundary, so
code that was previously pinned to the bytecode interpreter now runs as native
machine code:

- **Struct-receiver methods.** A method whose receiver is a struct of scalar
  fields runs natively whether it takes `&self`, `&mut self`, or `self` by
  value; a `&mut self` method's in-place field changes are written back into
  the caller's binding.
- **Nested integer vectors.** A function taking a vector of integer vectors
  crosses the boundary in the runtime's native layout, built once per source
  value and reused across repeated calls on the same value.
- **Recursive enum parsers.** A function returning a `Result` of a recursive
  enum - including variants that carry vectors and string-keyed pairs - now
  promotes to native code, so a hand-written recursive-descent parser runs
  natively through its whole recursion.
- **Recursive enum transforms and serializers.** A function that consumes a
  recursive enum by value and returns a freshly rebuilt one, and a function
  that reads a recursive enum by reference while writing through a
  `&mut String`, both run natively.

This is acceleration only: results stay bit-identical across the bytecode VM,
the in-process JIT, and the AOT compiler.

A `Result<Enum, _>`-returning body hands its two-word `[disc, payload]` carrier
back to the trampoline through an out-pointer wrapper rather than an `i128`
return value: a pointer argument has the same ABI on every target, where an
`i128` return lands in a register the Windows x64 ABI and a Rust `extern "C"`
shim disagree on. The carrier marshalling now matches across the bytecode VM,
the in-process JIT, and the AOT compiler on every platform.

The in-process JIT's calls *into* the runtime now use the same Windows x64
`i128` convention as the runtime itself. The Result / Option carrier helpers
(`gos_rt_result_new` / `_disc` / `_payload`, the `option_*` / `result_*` /
`iter_*` combinators, `gos_rt_debug_option` / `_result`, `http::serve`) take and
return the two-word `[disc, payload]` carrier as an `i128`; on
`x86_64-pc-windows-msvc` a Rust `extern "C"` function passes such an `i128`
argument by pointer and returns one in a vector register, where Cranelift's bare
`i128` uses integer register pairs. The JIT previously emitted the bare `i128`
call, so the carrier decoded to a wild pointer and faulted; it now spills the
argument to a 16-byte slot and passes its address, and reads the return through
the vector register, exactly as the AOT compiler already did. (Affected
`gos` on Windows only; the bytecode VM and AOT tiers were always correct.)

When the in-process JIT is implicated in a crash, two knobs aid diagnosis:
`GOS_JIT_ONLY=<fn,fn>` promotes only the named bodies and `GOS_JIT_SKIP=<fn,fn>`
promotes all but the named ones (others run on bytecode), so a single run can
isolate which body's native code is responsible; and a hard fault inside a
JIT-compiled body now prints the body's name, the fault address, and the
faulting instruction pointer instead of an opaque exit code. On Windows this
runs from a first-chance vectored exception handler: JIT-compiled code carries
no unwind metadata, so the stack walk an unhandled-exception filter relies on
aborts before the filter is reached, but a vectored handler runs before any
dispatch.

### Fixes

`Vec::is_empty` and `String::is_empty` now correctly report whether the
collection is empty; they previously always answered `false`.

A collection built in a local and then moved into an enclosing value is now
kept alive through the move on the compiled tiers - previously a vector of
key-value pairs moved into a parent could be released before the parent
escaped, yielding wrong results or a crash.

### Docs

Refreshed docs to more accurately reflect idiomatic Gossamer.

## 0.21.0 - Comptime, operator overloading, structural comparison, generic structs

### Comptime - compile-time evaluation

Zig-style `comptime`: ordinary Gossamer evaluated on the bytecode VM during
compilation and folded to a literal, so the bytecode VM, the Cranelift JIT,
and the LLVM AOT backend all compile the identical constant - comptime never
reaches a backend, and tier parity is automatic. No macro grammar, no hygiene
model, no token-tree DSL.

- **`comptime { ... }` blocks and `comptime fn` calls.** A `comptime` block
  evaluates its body at compile time; every call to a `comptime fn` folds at
  the call site. A region may use the full language (`let`, loops, `if` /
  `match`, calls, string building) as long as every value it reads is
  compile-time-known; referencing a runtime binding is a compile error. Results
  fold to a scalar or `String`. This also makes `const T: i64 = comptime {
  recursive_fn() }` compile natively, where the bare non-inlinable
  `const T = recursive_fn()` form does not.

- **`comptime` parameters.** A parameter declared `comptime` has its argument
  evaluated at compile time and replaced with the result literal, while the
  function runs normally: `fn scale(comptime factor: i64, x: i64)` folds
  `scale(BASE * 2 + 5, x)` to `scale(205, x)`. A non-comptime-known argument to
  a `comptime` parameter is a compile error.

- **Reflection: `typeInfo::<T>()`.** Reflects a struct's fields at compile time
  as `[(name, type)]`, so a `comptime fn` can generate per-type code - e.g. a
  SQL `CREATE TABLE` string built from the reflected columns, embedded in the
  binary as a constant.

- **Build-time validation: `regex!` / `sql!`.** Validate their argument at
  compile time and fold to the validated string; a malformed pattern or
  statement fails the build with a diagnostic rather than reaching runtime.
  These are the only compile-time-validation macros - every other `name!(...)`
  outside the six format macros remains a parse error (`GP0001`).

  Wired across the bytecode VM, Cranelift JIT, and LLVM AOT (new
  `feature-testing-examples/comptime_fold.gos`, `comptime_reflection.gos`, and
  `comptime_params_validate.gos` tier-parity fixtures).

### Expressiveness and footgun fixes

- **Arithmetic operator overloading.** A user struct or enum that implements
  `Add` / `Sub` / `Mul` / `Div` may be combined with `+` / `-` / `*` / `/`;
  the operator routes to the impl method and the result is the method's return
  type, so a dot product (`impl Mul for V2 { fn mul(self, o: V2) -> f64 }`)
  types correctly. Applying an arithmetic operator to an ADT with no matching
  impl is now a compile error (`GT0003`) instead of a runtime
  `unsupported value kinds` panic. Routed on the bytecode VM, Cranelift, and
  LLVM (new `feature-testing-examples/operator_overload_arith.gos` parity
  fixture).

- **Byte literals compare against the byte index without a cast.** `s[i] ==
  b'>'` type-checks: a byte literal compared with an integer operand coerces to
  that operand's type, so byte-level parsing reads `b'A'..=b'Z'` instead of
  magic ASCII integers. A byte literal is an `Int` value on every tier, so the
  comparison is unchanged downstream (new `byte_literal_compare.gos` fixture).

- **`from_json` infers its type from the binding annotation.** The turbofish is
  now optional when the target type is known: `let u: User = from_json(&t)?`
  and `let r: Result<User, E> = from_json(&t)` both resolve without
  `::<User>`. Explicit turbofish still works (new `from_json_infer.gos`
  fixture).

- **Malformed format placeholders are rejected.** A format macro placeholder
  whose name is an expression rather than a binding - `println!("{age + 1}")` -
  is now a parse error (`GP0021`) instead of being emitted silently as the
  literal text `{age + 1}`. Binding names, positional `{}`, and `{:spec}` /
  `{name:spec}` are unaffected.

- **Structural comparison of aggregates.** `==` / `!=` on fixed arrays and
  `Vec<T>`, and the full set of ordering operators (`< <= > >=`) on tuples,
  now compare element-wise instead of by identity. `[1, 2, 3] == [1, 2, 3]` is
  `true` (was `false`); `(1, 2) < (1, 3)` is `true` (previously a runtime
  panic). The VM walks the values; the compiled tiers route to new
  `gos_rt_tuple_cmp` / `gos_rt_vec_eq` runtime helpers. Tuple ordering is
  lexicographic over scalar and string elements (new
  `feature-testing-examples/aggregate_compare.gos`).

- **`sort_by_key` / `sort_by_key_desc` Vec methods.** Sort a `Vec` by an
  extracted key, ascending or descending. The key may be a tuple, so multi-key
  sorting is `xs.sort_by_key(|e| (e.count, e.name))` with no `Ordering` trait.
  Wrapping a key element in `Reverse(...)` flips just that element's direction,
  so `xs.sort_by_key(|e| (Reverse(e.count), e.name))` sorts by count descending
  then name ascending in a single pass. The desugar inlines the key into a
  `sort_by` comparator that orders with `<` (flipping `Reverse` elements),
  identical on every tier (new `feature-testing-examples/sort_by_key.gos`).

- **Fix: closures returning an aggregate by value.** A closure / `Fn` value
  returning a tuple / struct / array produced garbage on the LLVM AOT tier -
  the indirect-call site stored the result's box pointer instead of copying
  the aggregate's slots. It now materializes the aggregate like a direct call
  (the VM was already correct), so `let g = |n| (n, n * 10); g(1)` round-trips
  on `gos` and `gos build`.

- **Recursive generic functions over user structs compile.** A self-recursive
  generic instantiated with a struct (`fn rec<T>(v: T, n: i64) -> T { if n <= 0
  { v } else { rec(v, n - 1) } }` with `T = MyStruct`) was rejected by the
  flat-i64 ABI check (`GM0001`): the template's self-call carried the type
  parameter `T` as its generic argument, which the check mistook for a concrete
  oversized instantiation. A `Param`-typed generic argument is now recognized as
  a template-internal reference, not a concrete instantiation, so recursive
  generics over structs compile and run on every tier.

- **Fix: a generic call result used inline keeps its instantiated type.** A
  generic function's result used directly in a format macro
  (`println!("{}", id(s))`) printed garbage on the compiled tiers - a `String`
  or `f64` result rendered as a raw integer - while the VM stayed correct.
  MIR lowering was overriding the call expression's already-instantiated type
  with the callee's raw declared return type, which for a generic function is
  the un-instantiated type parameter (`Param(T)`); the codegen then chose the
  formatter for `i64`. The declared return type is now used only to ground an
  unresolved call type and never when it carries a `Param`, so the instantiated
  result type (`String` / `f64` / a struct) reaches codegen unchanged. Scalar,
  string, float, struct, multi-parameter, and recursive generic results all
  format identically across `gos` and `gos build` (new
  `feature-testing-examples/generic_call_result.gos`).

- **Generic struct types and their methods.** A struct that holds its type
  parameter by value - `struct Wrapper<T> { value: T }` - and `impl<T>
  Wrapper<T> { ... }` methods on it now compile and run identically on the
  bytecode VM, Cranelift, and LLVM tiers. Each instantiation lays the field out
  by its concrete type (`Wrapper<Point>` stores a whole `Point` inline; before,
  the field stayed an opaque `Param` slot and the compiled tiers crashed), and a
  generic method (`fn get(&self) -> T`) is specialised per receiver type so its
  return is the real type rather than a raw pointer. Covers scalar / string /
  float / struct payloads, multiple type parameters (`Pair<A, B>`), nested
  generic structs, and arrays of generic structs. The type checker now brings an
  `impl<T>`'s generics into scope for each method (so `-> T` records a rigid
  `Param` matching the struct's generic), the monomorphiser registers a
  per-instantiation field-type table and specialises each generic method by
  receiver type, and every layout / field-projection site reads the substituted
  fields (new `feature-testing-examples/generic_struct_types.gos`). Methods on a
  generic struct still require the explicit `impl<T> Wrapper<T>` form, as in Rust.

## 0.20.1 - Interpreter in-place growth

The bytecode VM now grows collections in place across the cases where it
previously rebuilt them, so a `String` / `Vec` assembled element-by-element in a
loop costs time proportional to the data, not its square.

- **Tail-position in-place mutation.** A discardable `v.push(x)` (and `insert` /
  `remove`) lowers to its dedicated in-place op even when it is the tail of an
  `if`, `match` arm, or block - not only a top-level statement. The
  value-discarded context now propagates into those control-flow tails, so the
  push grows the backing buffer with amortized capacity instead of routing
  through the value-returning builtin that copies the whole collection per call.

- **In-place `String` append.** `s += rhs` (and `*out += rhs` through a
  `&mut String`) appends onto the string's existing buffer via a new
  `Op::StrAppend`, keeping spare capacity for amortized O(1) growth. The previous
  lowering concatenated into a fresh string and stored it back, copying every
  byte on each append.

- **`&mut` arguments move into their write-back cell.** A `&mut <local>`
  argument is moved (rather than cloned) into its write-back cell when no sibling
  argument reads the same local, and the cell's post-call value is published back
  by moving it into the caller's home register. The callee therefore holds the
  collection uniquely and mutates it in place; a sibling read of the same local
  falls back to a clone so it still observes the pre-call value, matching the
  compiled tiers. This makes a recursive `&mut String` / `&mut Vec` accumulator
  (a serializer, a graph walk) grow linearly under `gos`.

These are interpreter-tier performance changes only; output is bit-identical
across the bytecode VM, the Cranelift JIT, and the LLVM AOT tier (new
`feature-testing-examples/inplace_mut_append_parity.gos` parity fixture).

## 0.20.0 - Router pipe-chaining, Memory and GC Improvements, Documentation

HTTP route registration now composes as a `|>` pipeline. The router verb
methods (`get`, `post`, `put`, `delete`, `patch`, `head`, `options`, and their
bare-function `_fn` variants) return the router they were called on, so a route
table reads as one left-to-right expression instead of a sequence of mutations:

```gos
let r = router::Router::new()
    |> _.get("/", home)
    |> _.post("/items", create_item)
http::serve("0.0.0.0:8080", r)?
```

- **Chainable verb methods on every tier.** The router pointer is threaded back
  through the C ABI shims, the ABI registry, the Cranelift and LLVM dispatch
  paths, MIR return-type and destination-kind inference, and the bytecode-VM
  builtins, so chaining is bit-identical across `gos`, the in-process JIT,
  and `gos build`. A new `http_router_chain.gos` tier-parity fixture pins the
  behaviour. The mutating form (`r.get(...)` as a statement) still works.
- Path parameters are unchanged: read them from the request with
  `r.path_value("name")` / `r.path_int("id")`.
- Examples, the standard-library router reference, and the skill card now show
  the pipe-chained style.

Memory and GC:

- **Perceus-style in-place reuse.** When an owned local is released and a
  same-type enum is constructed nearby, the compiled tiers recycle the dropped
  block in place (`gos_rt_rc_drop_reuse` + `gos_rt_rc_alloc_reuse`) instead of a
  free + fresh allocation - so a loop that reassigns a heap value
  (`node = Variant(..)` each iteration) does no allocation churn. A runtime
  refcount check keeps it safe: reuse happens only when the block is the unique,
  thread-local, weak-free owner, otherwise it falls back to a normal release +
  allocation, so it can never corrupt - only forgo the optimization. Reuse is
  observationally transparent and bit-identical across tiers (the bytecode VM
  does not reuse); `GOS_RC_NO_REUSE` disables it.
- **`[u8]` / `Vec<u8>` are byte-packed (stride 1).** A byte buffer now costs one
  byte per element like Go's `[]byte`, not an 8-byte word per byte. A
  never-evicted `HashMap<i64, [u8]>` cache dropped from ~328 to ~114 bytes per
  entry (2.85x), matching Go and best among the compared languages. Reads
  zero-extend, so values above 127 round-trip exactly; bit-identical across the
  bytecode VM, JIT, and native tiers (new `byte_vec_packed` fixture). Helps all
  binary / IO / network buffers.
- **Cyclic garbage collection is incremental.** The automatic cycle collector
  processes a bounded slice of candidate roots per run (with buffer
  reconciliation) and adapts its trigger threshold to how much it reclaims, so a
  churn of live shared graphs no longer pays a full scan and one collection can
  never stall the goroutine on an unbounded sweep. Explicit
  `runtime::collect_cycles()` still fully drains.
- **Faster region allocation.** The arena-region bump path takes a single
  thread-local probe instead of two; allocation-heavy region code.
- **Deep structures tear down without overflowing the stack** on the
  interpreter tier: dropping a million-deep list / tree / graph is iterative
  past a depth threshold, matching the native tier's robustness.
- **`GOS_RC_DEBUG` reports cross-goroutine leaks.** The exit line now includes a
  live shared-object count and points at `Weak<T>` when a shared reference cycle
  (the one class the per-goroutine collector cannot reclaim) is leaked.
- Internal: the guarded-aggregate provenance set is sharded to remove a global
  lock from the struct-copy alloc/free path.

Documentation and tooling:

- Dropped the "systems language" positioning from the docs, landing page, and
  skill card; Gossamer is described as a goroutine-powered, fast-compiling
  language.
- Corrected the Python migration guide's set-comprehension example to
  deduplicate with `HashSet` and sort alphabetically.
- Fuzz CI forces HTTP/1.1 for crate fetches (`CARGO_HTTP_MULTIPLEXING=false`),
  avoiding a transient curl HTTP/2 framing error on some runners.

## 0.19.1 - Soundness hardening: checked `arena { }`, cycle coverage, JIT correctness

The `arena { }` block's escape contract is now enforced at compile time. A
value allocated inside an `arena { }` block that is used after the block
exits - a use-after-free - is rejected by the front-end with `error[GM0003]`
on every gate (`gos check`, `gos`, `gos build`, `gos test`) and in the
editor through the LSP. Previously the contract was the programmer's to uphold;
it is now statically verified, so the ergonomic `arena { }` surface is
memory-safe by construction.

- **Static arena-escape analysis.** A conservative front-end pass tracks which
  values are arena-allocated and reports any that reach a sink able to outlive
  the block: assigning to a binding declared outside the block, pushing into a
  container that outlives it, sending on a channel, returning, breaking out of
  an enclosing loop, capturing in a goroutine/closure, or passing into a
  function that may stash the value. Reading an arena value through a method or
  a region-safe free function (`check(&tree)`) stays allowed, so idiomatic
  build-and-discard code is unaffected.
- The analysis is sound by over-approximation: when it cannot prove a shape
  safe it rejects, so it may ask you to restructure a sound program but never
  accepts an escaping one within the sinks it models. Run `gos explain GM0003`
  for the catalogue entry.
- The raw `runtime::arena_push()` / `runtime::arena_pop()` primitive is left
  unchecked, as the low-level escape hatch for shapes the block does not fit.

Cycle-collection hardening:

- **Cyclic reclamation is now covered by a cross-tier fixture.** A new
  `cycle_reclaim.gos` builds a real reference cycle each round, lets the
  external handles die, and reclaims it with `runtime::collect_cycles()` -
  the trial-deletion path that pure reference counting cannot free. Its
  output is bit-identical across VM, Cranelift, and LLVM, extending the
  test coverage that previously lived only in the runtime's Rust unit tests.
- **Documented the one weak-reference cross-tier caveat.** A `Weak` that
  observes a member of a *strong* cycle reads as live on the interpreter
  (whose collector is a no-op) but as `None` on the compiled tiers after
  collection. The idiomatic weak-to-break-a-cycle pattern is unaffected, as
  it forms no strong cycle. See the memory-management docs.

Interpreter JIT correctness and promotion:

- **Fixed an interned-string alignment bug that corrupted memory under the
  JIT.** Static string literals were emitted with no alignment, so a literal
  whose body fell on an even address defeated the reference-counting
  accounting - which distinguishes a string body from a tagged pointer by
  its low bits - and wrote into read-only memory. Aligning the literals
  keeps every string body at the odd address the runtime expects. This is
  what lets aggregate-parameter functions (a `Vec` argument, including
  slice-pattern matches) JIT correctly rather than being held back.
- **The JIT now refuses bodies it cannot lower, instead of miscompiling
  them.** A function that passes a closure to a higher-order call or takes a
  `&mut` aggregate parameter is kept on the bytecode interpreter rather than
  promoted to native code that returned a wrong result or crashed. These
  miscompiles were latent - the previous call-count promotion threshold
  rarely reached them, but any sufficiently hot such function would. A new
  test promotes every function on its first call and asserts the output
  matches the bytecode interpreter, locking the eligibility gate in place.
- **Hot loops in rarely-called functions now reach native code.** A
  loop-bearing or recursive function the JIT can lower is compiled on its
  first call rather than only after a call-count threshold, so a hot loop
  inside a function called once or a handful of times is no longer stranded
  on the interpreter. Promotion is gated by the eligibility check above, so
  it never promotes a body the codegen would miscompile.
- **More value shapes cross the JIT boundary.** Functions taking or
  returning `Vec<f64>`, `Vec<(i64, f64)>`, or a `U8Vec` byte-buffer handle
  are now marshalled natively through the JIT trampoline, joining `String`
  and `Vec<i64>`. The eligibility rule is derived from the marshaller itself
  rather than a hand-maintained type list, so a body is held back only when
  the marshaller genuinely cannot classify one of its values - it never
  strands a function over a local the boundary already understands.
- **Hybrid interpreter/JIT output keeps program order.** When a JIT-promoted
  function writes to stdout through the runtime buffer while the surrounding
  bytecode prints through the interpreter, the two streams are now emitted in
  source order: the interpreter drains the runtime buffer before each of its
  own writes. Previously a program that interleaved `print!` with a native
  helper's direct writes could surface them out of order.

## 0.19.0 - VM to WASM: In-browser support, automatic arenas extension

The bytecode VM now compiles to WebAssembly and runs Gossamer in the browser.

- **In-browser playground.** The bytecode VM compiles to `wasm32-unknown-unknown`
  and runs Gossamer entirely client-side, powering the runnable home-page
  examples, a guided Tour, and a standalone Try Gossamer editor. The whole
  language and the pure standard library - including hashing
  (`sha256`/`sha512`/`blake3`/`hmac`) - run bit-identical to native;
  browser-sandbox I/O (sockets, filesystem, processes, HTTP server and client,
  SQL, signing/AEAD crypto, `zstd`/`bzip2`) is unavailable, and goroutines run
  cooperatively to completion (`spawn`/`join` and channel drains work, preemptive
  interleaving does not). Standalone `gos build --target wasm32` stays
  unsupported. See the WebAssembly docs.
- An execution budget caps runaway loops in the playground: an unbounded loop
  aborts with `error[GX0009]` instead of hanging the tab. Feature-gated, so
  native `gos` carries no overhead.

### Automatic arenas now cover `for` loops and map iteration

- The compiler's automatic arena regioning - which wraps a loop body whose
  allocations provably die at the iteration boundary so the iteration's heap is
  bulk-freed instead of torn down node by node - now applies to `for` loops
  (`for i in a..b`, `for x in xs`, `for (i, x) in xs.enumerate()`), not only
  `while`. Allocation-churn code written the idiomatic way
  (`for _ in 0..n { let t = build(); use(&t) }`) gets the same speedup the
  `while` form already had: a balanced-tree build-and-discard loop runs ~3-4x
  faster on the compiled tiers, with output bit-identical across every tier.
  The eligibility check is unchanged and conservative - when it cannot prove
  every allocation stays inside the iteration, the loop keeps the ordinary
  reference-counted path, so the change can only speed code up, never alter a
  result.
- The same regioning now also covers `for (k, v) in m.iter()` over `HashMap`
  and `BTreeMap` (including struct- and tuple-keyed maps) and bare `loop { }`
  bodies, so every loop form an allocation-churn body might use takes the
  bulk-free fast path under the one conservative escape check. Coverage only -
  the eligibility gate is unchanged, and output stays bit-identical across
  every tier.
- `GOS_ARENA_TRACE=1` reports, per loop, whether the body was auto-regioned,
  and when an allocating loop was not, the reason (a method call, an escaping
  value, a nested loop, an early exit, ...) plus a hint to wrap the body in
  `arena { }`. Turns an unexpected slow path into a named, actionable signal.

### Indexing speedups (compiled tiers)

- A `[bool]` element reads and writes through a constant 1-byte stride instead
  of loading the element width from the vector header and branching on it at
  every access. Random-access bool work (visited-sets, bitmaps) gets faster.
- A bounds check uses one unsigned comparison (`index >= len` catches both a
  negative and an over-length index) in place of two signed comparisons, for
  every checked vector and fixed-array access.

## 0.18.3 - Compiled-tier hot-path performance, parity, correctness

Compiled- and interpreter-tier hot-path speedups and tier-parity fixes.
Output stays bit-identical across the VM, the Cranelift JIT, and the LLVM
AOT tier.

- Ordinary `HashMap` values are goroutine-local and take no per-operation lock;
  a map only locks once it escapes to another goroutine (codegen marks it shared
  at the `go` / channel escape points, like RC values). Genuinely shared maps
  stay fully synchronized - safer than Go's unsynchronized maps - at zero cost
  otherwise.
- Statement-position `go f(args)` now marks its escaping args shared (matching
  expression-position `go`): flips a shared map onto its lock and switches a
  passed `String` / struct to atomic reference counting.
- String-keyed map ops and `String.substring` read their length from the O(1)
  string header instead of a per-call `strlen` (a sliding-window substring scan
  was O(n^2)).
- The runtime keeps short string and map-key copies inline (overlapping
  word loads/stores) instead of calling libc `memcpy`, and string allocations
  zero only their trailing NUL rather than memset-ing the whole buffer. The
  static-musl `--release` link resolves `memcpy` / `memset` to musl's scalar
  routines, whose per-call overhead dominated the small per-k-mer copies; the
  inline path removes it without giving up the portable static binary.
- `m.iter()` on a `&HashMap` parameter peels past `&` before dispatch, so a
  borrowed map yields real entries instead of a garbage-length vec / hang.
- The LLVM backend and Cranelift JIT inline word-stride `Vec<f64>` (and nested
  `Vec`) get/set off the GosVec header, not just integer elements, so `opt -O3`
  can hoist them (spectral-norm `Vec<f64>` drops an order of magnitude).
- The VM's typed flat-local reads (`IntArrayGetI64`, `FloatVecGetF64`) return
  the lenient zero on an out-of-range index, matching the compiled tiers.
- The bytecode VM gains fused super-instructions for `String.substring` and
  `m.inc` (the sliding-window counter pattern), bypassing the per-call
  method-dispatch + receiver clone + map lock-handle round-trip; `substring`
  also builds its result inline with no intermediate owned `String`.
  k-nucleotide `gos` drops ~50% (5.28s -> 2.66s, matching CPython).
- Building from source / CI retries a transient crates.io index resolution
  failure up to 10 times (`net.retry` in `.cargo/config.toml`) instead of the
  default 3, riding out brief registry DNS / timeout blips on slow runners.

## 0.18.2 - Interpreter memory improvements

Reduces `gos` (bytecode VM) peak memory. Output stays bit-identical
across the VM, the Cranelift JIT, and the LLVM AOT tier.

- The in-process JIT compiles a function only when it does real work per
  cross-boundary call (it has a loop or it recurses); a program with no
  such function stays on the bytecode path instead of faulting in the
  Cranelift compiler.
- `HashMap<String, i64>` stores unboxed `(SmolStr, i64)` entries and
  probes by borrowed key, dropping the per-entry key tag and the boxed
  count.
- Enum and struct payloads of small arity are stored inline in the
  value's heap block rather than in a separate buffer.
- A spawn-free program releases its lowered MIR and type-context
  snapshot once the deferred JIT settles, rather than holding them until
  exit.
- The `GosStruct` derive no longer trips the unstable `str_as_str`
  feature; the generated field lookup slices the struct's fields
  directly.

## 0.18.1 - Stability and minor performance sweep.

A stability sweep that closes the programs which still passed `gos check` on 0.18.0 and then segfaulted, printed uninitialised memory, or diverged across tiers - plus the memory leak and use-after-free found alongside them.

### Safety and correctness - `gos check` rejects what crashed

- **Non-exhaustive `match` over a non-enumerable scrutinee is rejected.** A `match` over `i64` / `String` / `char` / `f64` with no wildcard arm was treated as exhaustive, then ran off the end of the dispatch and segfaulted on the compiled tier. Such a match now requires a catch-all (`GM0001`).
- **`Option` / `Result` matches must cover their variants.** `match o { Some(n) => .. }` with no `None` arm passed check and read an uninitialised discriminant on the compiled tier (garbage / a wild pointer); it is now reported non-exhaustive. A guarded-only `Some` arm no longer counts `Some` as covered.
- **Indexing, calling, and tuple access on the wrong type are rejected** instead of falling through to a fresh inference variable: indexing a non-`[T]`/`Vec`/`String` value (`GT0021`, was a compiled segfault), calling a non-function value (`GT0022`, was a build failure), and `.N` on a non-tuple or past a tuple's arity (`GT0023`, was an out-of-object read). Inference variables (e.g. an unsuffixed `let x = 5` used before defaulting) are re-checked after defaulting, so `x[0]` / `x(3)` on an integer are caught.
- **Method calls are arity-checked and typo-checked.** A method called with the wrong number of arguments (`GT0018`, the compiled tier zero-filled the missing one) and a method that no type declares (`GT0002`, the compiled build failed on an undefined symbol) are now rejected. A piped `x |> recv.m(a)` correctly counts the implicit argument.
- **A `match` that slips past exhaustiveness panics cleanly instead of corrupting memory.** The compiled tiers lowered a non-matched `match` to `unreachable` (undefined behaviour - a segfault on LLVM, a trap on Cranelift); both the switch default and the guarded fall-through now emit a clean panic, so an exhaustiveness blind spot can never be memory-unsafe.
- **Use-after-free fixed: the weak-reference count now saturates** instead of wrapping at 256. 256 live `Weak`s to one object wrapped the `u8` count to zero and freed a block still observed by weaks; the count now pins at its maximum (leak-rather-than-corrupt), matching the strong-count policy.
- **Memory leak fixed: a `String` / RC field nested inside a by-value sub-struct is now released** when the outer struct dies. The per-field RC teardown walked only the outer struct's direct fields, so `Outer { inner: Inner { s: String } }` leaked `inner.s` every iteration (RSS grew to ~128 MB at 2M iterations, now bounded at ~4 MB). The teardown now recurses through by-value sub-structs and tuples, with matching recursive retains at every site that shares a nested pointer - whole-struct copy, aggregate operand, functional record update (`Type { ..base }`), and sub-struct field extraction - so a nested share is freed exactly once and never double-freed.
- **VM handle registries shared across goroutines no longer lose state.** `HashSet`, `VecDeque`, the HTTP router table and its handlers, compiled-regex handles, and the TCP / TLS / UDP / Unix socket registries were stored per-thread, so a handle created before a `go` / channel hand-off vanished when the goroutine resumed on another worker. They now use a process-global registry. Sockets additionally hold each connection behind its own `Arc<Mutex<_>>` and clone it out under a brief registry lock, so blocking I/O never holds the registry lock - a global socket registry would otherwise deadlock when the scheduler parks a goroutine mid-read.
- **`for (_, v) in m.iter()` over a map with struct values is fixed on the compiled tiers.** A `_` wildcard in the for-loop tuple-destructure dropped out of the optimised map-iteration path, so the struct value was read as a raw pointer (garbage, or an earlier `gos build` ICE) while the VM ran it correctly. The wildcard now takes the same path as `(k, v)`, binding the value as a box reference; `(_, v)`, `(k, _)`, and `(_, _)` over any key/value type match the VM bit-for-bit.
- **Returning a const-generic array no longer corrupts memory.** A `fn f<const N: usize>(xs: [T; N]) -> [T; N]` that returned its argument produced garbage and then a use-after-free segfault on the compiled tier: the value was carried as a runtime sequence everywhere except the return path, which read the heap pointer as the first element. The `[T; N]` return now takes the same `Vec` representation as the parameter across the checker, the callee's return slot, and the call-site type.
- **A closure capturing a struct or tuple reads every field, not just the first.** The capture stored only the aggregate's first word in the closure environment while the body read it back as a pointer, so any field past the first was uninitialised memory (garbage / a stray address). The aggregate is now copied into a stable heap box at capture; the body materialises it by value, and a captured value survives an escaping closure.
- **`for (_, v) in m.iter()` over a struct- or tuple-keyed map iterates its values on the compiled tier.** A struct key hashes to opaque bytes the runtime cannot turn back into a value, so the key-driven loop never iterated and the sum came out zero while the VM ran it correctly. The loop is now driven from the values snapshot for scalar values, matching the VM. (Key access and String/struct values over a struct-keyed map remain unsupported - a struct key still does not round-trip.)
- **`{:?}` of an `Option<T>` / `Result<T, E>` builds on the compiled tiers.** Debug-formatting a built-in by-value enum was rejected by `gos build` (only user types with a derived `fmt` were routed); `Some(5)` / `None` / `Ok(7)` / `Err(e)` with scalar or `String` payloads now render through a runtime helper exactly as the VM prints them, on both the LLVM and Cranelift tiers. A struct or nested payload stays a clear build error rather than silent garbage.
- **An observed goroutine panic no longer prints a spurious report.** A panic in a `spawn`ed goroutine that is caught through its `join()` handle delivered the error twice on the compiled tier - once as `Err(message)` to the joiner and once as an `error[GX0005]` report plus a native trace on stderr - while the VM reported only the `Err`. The compiled tier now suppresses the eager report for a joinable body (the handle owns the error) and keeps it for a fire-and-forget `go`, matching the VM.

### Performance

- **Struct field names are interned, not heap-allocated per instance.** `Value::Struct` stored every field name as an owned `String` (one heap allocation per field per value); names are now `&'static str` interned once at program load, so construction copies cached pointers with no per-value allocation or lock. Struct-heavy interpreted workloads shed RAM.
- **Scalar- and string-keyed `HashMap` entries are 24 bytes smaller.** The rarely-used aggregate-key arm of `MapKey` widened every key to 40 bytes; it is now boxed, returning int- and string-keyed maps to a 16-byte key.
- **Tight loops drop a dead per-iteration unit load.** A loop body whose last line was a discarded expression (an assignment, an in-place mutation) emitted a boxed unit `LoadConst` that nothing read, every iteration.
- **`&mut self` method calls mutate in place instead of deep-cloning.** A `&mut self` call on a local receiver wrapped it in a write-back cell whose refcount inflation forced the first field write to copy-on-write the whole struct. The receiver is now moved through the cell (refcount stays one), so the mutation lands in place; an aliased copy still triggers the copy, preserving value semantics. faster on method-dispatch-heavy loops, bit-identical across tiers.

### Documentation

- `SKILL.md` corrected: version, the `std::fs` surface (removed ~11 functions that are not exported to Gossamer), and the out-of-range-index contract (scalar elements yield a zero value; aggregate elements panic).

## 0.18.0 - Authoritative checking, crash fixes, stdlib cleanup, syntax

Closes the gap between `gos check` and what runs: a program that type-checks now compiles and runs with the same meaning on every tier, `check` rejects what the runtime or native backend would, and the memory-safety crashes that survived `check` are gone.

### Safety and correctness

- **`os::args()` returns owned refcounted strings, not raw `argv` pointers.** The compiled tiers wrapped libc's `argv` directly, so reference-counting on an arg (`.clone()`, drops) read an RC header off a raw pointer - corrupting an adjacent argument or freeing a libc pointer (segfault). Each arg is now a gos-tagged copy.
- **Recursive `Box`-enum cloned in a loop no longer double-frees.** Move-elision dropped the balancing retain on a loop-invariant source read each iteration, so the enum's `Box` children were over-released (heap corruption / exit crash, deterministic under goroutine capture). Loop-invariant sources now keep the retain.
- **More compiled-tier segfaults on ordinary programs fixed:** `Vec<Struct>::new()` + push (the backend truncated the element width to 8 bytes), `HashSet<i64>::insert` (a raw i64 passed as a key pointer), and a bound `regex::find_all` result iterated (a declared-vs-runtime return-type mismatch strode 8-byte slots over 24-byte tuples).
- **Out-of-range indexing is consistent on every tier.** Aggregate `Vec` reads and `v[i].field` writes panic with `index out of bounds` (were a compiled segfault / VM error); whole-element, scalar, and scalar fixed-array reads/writes are a lenient no-op / zero value matching the VM and the documented contract.
- **`String` refcounting is atomic once shared across goroutines** (a string escaping to another goroutine sets a shared bit and uses atomic retain/release), closing a clone/drop data race; the non-shared fast path is unchanged.
- **`option`/`result` `and_then`/`or_else` closures returning a struct payload now agree across tiers on Windows.** The runtime invokes the callback through `extern "C" fn(..) -> i128`, whose Win64 ABI returns the 2-word `Option`/`Result` in a vector register, but a gossamer closure returned it in the GP-register pair - so a callback that built a new struct option (whose reference-counting clobbered the vector register) was read back as a garbage discriminant and silently dropped. The callback's address is now taken as a vector-return thunk, the same bridge already used for HTTP handlers.
- A debug-build assertion catches RC underflow (double-free or a foreign pointer reaching RC dispatch).

### Compiler front-end - `gos check` is the authoritative gate

- **One shared front-end across `check` / `run` / `build` / `test` / `bench`**, replacing five drifted policies. `gos build` now runs exhaustiveness (a non-exhaustive `match` no longer compiles then segfaults), and diagnostics are no longer discarded.
- **Method, arity, and enum-variant calls are type-checked, not guessed** (the checker no longer falls through to a fresh inference variable). New `check` errors: method not on the receiver type or a free function called as a method (`GT0002`), wrong argument count (`GT0018`), unknown enum variant (`GT0019`), supertrait method through a generic bound (`GT0020`), wrong-typed `Vec` push. `String` `find`/`rfind`/`index_of` now type as `Option<i64>`, so `s.rfind(&"/").map(|i| i as i64)` matches across tiers (was native garbage).
- Non-canonical `std` import paths (`use std::json`, bogus paths) are rejected by `run`/`build`, not just `check`. Matching a value with the wrong constructor patterns (e.g. an `Option`-returning `env::var` with `Ok`/`Err`) is a check error rather than a silent tier divergence.
- `s.parse()`'s error type pins to `errors::Error`, so `{}` Display of an `Err` lowers correctly on the compiled tier (was a garbage char).

### Standard library

- **Tier-parity hardening across the stdlib.** A `stdlib_compiled_coverage` gate makes VM-only functions unrepresentable (fixed seven, including `crypto::sha512::digest` and `regex::replace`), and a broad differential sweep fixed value divergences in `strings::split`/`equal_fold`, `strconv` parsing (incl. `parse_u64` accepting negatives), `path::join`/`parent`, `time::parse_rfc3339`/`format_rfc3339`, JSON integer precision (large ints and integer-valued floats now round-trip exactly), and map `.contains(k)` (was VM-false). Coverage fixtures span strings, encoding, crypto, math, collections, iterators, and paths.
- **Collection dispatch completeness.** Iterating a `HashSet` directly (`for x in s`), `HashSet` `to_vec`/`iter`/`clear` and i64 elements, set-algebra results (`for e in a.union(&b)`), Vec method-form `insert`/`remove`, and `BTreeMap` (routed through the `HashMap` implementation) now build and behave identically on every tier.
- **`json::set(&mut obj, k, v)` persists fields** (it was a discarded functional call rendering `{}`); the functional form is unchanged.

### Cleanup

- Removed documented-but-broken entries (`std::strings` `to_lowercase`/`strip_chars`/`zfill`/..., the never-wired `std::sort` free functions) and redundant spellings (`utf8::count_runes`, `index_rune`/`contains_rune`, qualified `json::from_json`). Use `to_lower`, `trim_matches`, `pad_left`, the `.sort()`/`.sort_by()` methods, and the bare `from_json::<T>`.
- `String::from(s)` works as identity on every tier (was a VM error / compiled build failure).
- `vec![...]` is no longer accepted (Gossamer has six macros; `[...]` coerces to `Vec<T>`).
- `SPEC.md` reconciled with the implementation: the non-existent borrow-checker section, `dyn Trait`, raw-pointer types, and `Array<T, N>` removed; stale version markers cleaned up.

## 0.17.0 - TLS clients and servers, WebSocket, HTTP/3, multi-file packages, and Gossamer-native database drivers

### Networking

- **`net::TcpStream::start_tls(host)` upgrades a connected plaintext socket to a TLS client session.** A program can now perform a protocol's plaintext pre-handshake (e.g. PostgreSQL's `SSLRequest`) and then hand the same connection to rustls, getting back a `net::TcpStream` whose `read` / `write` / `read_to_string` / `close` drive the encrypted stream transparently - no separate stream type. Certificates verify against the webpki root store, the same trust anchors the HTTP client uses. Wired across the bytecode VM, Cranelift, and LLVM tiers (`gos_rt_tcp_start_tls`), so an upgraded stream behaves identically under `gos` and `gos build`. Bytes written to a TLS or plain stream go through the `[u8]` ABI; pass `s.as_bytes()` to send string content. Fixture: `net_tls_client.gos`.
- **TLS client verification modes for pure-Gossamer drivers: `start_tls_ca(host, ca_pem)` and `start_tls_insecure(host)`.** `start_tls_ca` verifies the server certificate chain and hostname against a PEM CA bundle you supply (PostgreSQL `sslmode=verify-full` against a private CA); `start_tls_insecure` encrypts without authenticating the peer (`sslmode=require`). With the public-root default `start_tls`, a pure-Gossamer client (e.g. a PostgreSQL driver) can connect to any TLS endpoint - managed, self-hosted, or self-signed. Both are wired across the VM, Cranelift, and LLVM tiers and behave identically under `gos build --release`. Fixtures: `net_tls_client_modes.gos`, `http_serve_tls_roundtrip.gos`.
- **`http::serve_tls(addr, cert_pem, key_pem, handler)` terminates HTTPS from Gossamer code.** The server builds a rustls config from a PEM certificate chain and private key and drives each accepted connection through the same request/response core as the plaintext server after TLS termination, so an HTTPS handler is written exactly like an HTTP one. Wired across all three tiers (`gos_rt_http_serve_tls`). A round-trip fixture exercises a real handshake plus all three client verification modes (skip-verify accepts, public-root verify rejects a private chain, custom-CA validates it). Fixture: `http_serve_tls_roundtrip.gos`.
- **The compiled HTTP/1.1 server emits `Date` and `Server`, honors `Expect: 100-continue`, and times out idle reads.** The `gos build` server now injects an RFC 1123 `Date` header and a `gossamer/<version>` `Server` header (unless the handler set them), sends `HTTP/1.1 100 Continue` before reading the body of a request that signals `Expect: 100-continue`, and applies a configurable per-connection read timeout (`GOSSAMER_HTTP_READ_TIMEOUT_MS`, default 30s) so a stalled peer cannot hold a connection thread - matching the interpreter tier. The TLS server shares the same request/response core, so it gets all of these too. Fixture: `http_server_headers.gos`.
- **Bidirectional WebSocket messaging: `websocket::serve` / `websocket::connect` plus `send_text` / `send_binary` / `recv` / `close`.** A self-contained WebSocket server upgrades each connection and hands the handler a connection it drives in a blocking recv/send loop; `websocket::connect(url)` opens a client. The RFC 6455 framing engine lives in a shared `gossamer-ws` crate used by both the interpreter and the compiled-tier runtime, so masked frames, fragmentation, and ping/pong behave identically across the VM, Cranelift, and LLVM tiers. `wss://` (WebSocket over TLS) is not yet supported. Fixture: `websocket_echo.gos`.
- **HTTP/3 (`http_h3::serve`) works on every tier.** The QUIC + h3 engine (quinn + h3, with a private tokio runtime) moved into a shared `gossamer-http3` crate that both the interpreter and the compiled-tier runtime use, so `http_h3::serve(addr, cert_path, key_path, handler)` is no longer interpreter-only - it links and runs under `gos build` and `gos build --release` (static-musl included). Dead-code elimination keeps the QUIC stack out of binaries that do not use HTTP/3. Fixture: `http3_serve_err_binding.gos`.

### Modules and packaging

- **Multi-file packages: subdirectory modules, nested modules, and `crate::` paths.** A library can now span files and directories. A sibling `src/<name>.gos` is the module `name`; a subdirectory `src/<dir>/mod.gos` is the module `dir`, including its own sibling files and nested subdirectories, recursively. A module reaches another via a navigation path - `super::other::item`, `crate::other::item` (rooted at the package), or `self::child::item` - with `crate::` now resolving the same as it does in Rust (previously only `super::` worked). The auto-bundler that assembles a package into one inline module tree drives `gos`, `gos build`, and `gos check` identically.
- **`gos` / `gos build` / `gos check` accept a project directory argument.** `gos my_project` (or `gos build my_project`) resolves the directory's conventional entry point instead of erroring with "expected a file, found a directory". `gos check <dir>` now type-checks the package as one bundled unit, so a valid cross-module reference like `crate::other::item` no longer reports a false unresolved-name error from checking each file in isolation.

### Language and formatting

- **Let-chains in `if` and `while` conditions.** An `if`/`while` condition may now be a sequence of clauses joined by `&&`, where each clause is either `let PAT = expr` or a boolean expression: `if let Some(x) = a && let Some(y) = b && x > 0 { ... }`. Earlier `let` bindings are in scope for every later clause and for the body, so `if let Some(inner) = pair && let Some(v) = inner` reads top-down without a nested `match`. An `else` attaches to the whole chain, and `while let` chains drain-and-test in one condition. A `let` clause chain is `&&`-only: joining `let` clauses with `||` (without parentheses) is a parse error (`GP0001`, "`let` in a condition can only be chained with `&&`"). A pure front-end desugar into nested `match`, so it runs bit-identically across the bytecode VM, Cranelift, and LLVM tiers. Fixture: `let_chains.gos`.
- **Open-ended range patterns in `match`.** `..=hi` and `..hi` (open start) and `lo..` (open end) join the closed `lo..=hi` and exclusive `lo..hi` forms. An open end covers up to the type's maximum (inclusive), so `1.. => "positive"` matches every value at or above `1`. Like closed ranges they are opaque to exhaustiveness, so a `_` arm is still required. An inclusive marker requires an upper bound, so bare `..=` and `lo..=` are parse errors. The patterns lower the same as closed ranges, so they run identically across the VM, Cranelift, and LLVM tiers. Fixture: `open_ended_ranges.gos`.
- **Irrefutable `let` destructuring on every tier.** `let Point { x, y } = p`, the renamed form `let Point { x: a, y: b } = p`, nested struct patterns (`let Nested { p: Point { x: nx, y: ny }, label } = nested`), enum / tuple-struct variant patterns (`let Shape::Pair(m, n) = s`), and irrefutable or-patterns (`let (Shape::Pair(g, _) | Shape::Single(g)) = v`, whose alternatives must bind the same names) now bind correct values on the bytecode VM, Cranelift, and LLVM tiers. These previously crashed `gos` or bound the wrong values under `gos build`. Fixture: `let_destructure_struct.gos`.
- Fixing stale println syntax in gos init and documentation.

### Generics

- **Const-generic array length is correct on every tier.** A `fn sum<const N: usize>(xs: [i64; N]) -> i64` parameter now monomorphizes with `N` inferred from the array argument's length under `gos`, `gos build`, and `gos build --release` (the const was previously threaded only through the VM and silently wrong on the compiled tiers). The const arg is keyed into monomorphization, so a `[T; N]` parameter iterates its real length, reports the right `.len()`, and may appear in the return type (`-> [i64; N]`); multiple const params (`<const N: usize, const M: usize>`) instantiate independently. Scope: the const is inferred from a `[T; N]` argument's length; it is not yet usable as a value expression in the body or as a repeat count (`[0; N]`). Fixture: `const_generic_array_len.gos`.

### Safety and correctness

- **Slice patterns over fixed-size `[T; N]` array literals no longer crash on `gos build`.** A `match xs { [first, ..rest] => ... }` over a fixed-size array (any element type, including `[String]`) materialized the `..rest` sub-slice with the wrong element size on the compiled tier, segfaulting at teardown. The lowering now binds prefix / suffix elements and the rest sub-slice with the correct element stride, so it matches identically across the VM, Cranelift, and LLVM tiers - the same as over `Vec<T>` / `[T]`. Fixture: `slice_pattern_fixed_array.gos`.
- **`String.byte_at(i)` is bounds-safe on every tier.** `s.byte_at(i)` now returns `0` for any index outside `[0, len)`, with no out-of-bounds heap access on the compiled tier (it previously read past the string's byte buffer). The read is bounded by the string's byte length on the bytecode VM, Cranelift, and LLVM tiers. Fixture: `string_byte_at_oob.gos`.
- **A free function that mutates a `&mut struct` / `&mut enum` parameter writes the mutation back to the caller on the VM.** Under `gos`, mutating a field of an aggregate `&mut` parameter (`fn fill(c: &mut Conn) { c.buf.push(b) }`) silently no-op'd while `gos build` wrote it back, a VM-only divergence: `&mut self` methods and `&mut Vec` / `&mut <scalar>` parameters already round-tripped, but aggregates were excluded from the write-back cell protocol. Aggregate `&mut` parameters now ride the same protocol, matching the compiled tiers (the receiver is passed by pointer there); fixed `[T; N]` arrays stay excluded. This makes wire-protocol-style code that threads a struct through helper functions behave identically on every tier.
- **A Gossamer-native database driver's per-connection handle round-trips on the interpreter.** `sql::native_set_handle` / `native_handle` (the one retained value a native driver stashes per connection - typically its goroutine's command channel `Sender`) stored the value as an integer on the interpreter, so a non-integer handle such as a channel came back as `0`. The interpreter now stashes the whole value, so the goroutine-per-connection pattern a real driver uses works under `gos` as it does compiled.
- **Sending a multi-field struct over a channel survives the cross-goroutine handoff on the compiled tiers.** `tx.send(Cmd { op, h, reply })` over a `Sender<Cmd>` stored a pointer to the sender frame's stack copy of the aggregate; the receiver, running on its own goroutine stack, then dereferenced a pointer that dangled the moment the sender frame was reused (misaligned-pointer abort or silent field corruption under LLVM and Cranelift). The channel-send lowering now heap-copies a by-value aggregate (RC-aware) so the channel carries a stable pointer the receiver owns, matching the `Ok`-payload path. Fixture: `chan_struct_payload.gos`.
- **`Sender<T>` / `Receiver<T>` / `JoinHandle<T>` written as a parameter or binding type resolve to their element type.** A `fn worker(rx: Receiver<Cmd>)` annotation fell through to a fresh inference variable, so `rx.recv()` defaulted its `Option<T>` payload to `i64` and a struct received from the channel materialised as a single pointer word instead of its inline fields. The type lowerer now recognises these channel handle constructors, and `recv` / `send` pin the channel element through the receiver. Fixture: `chan_struct_payload.gos`.
- **A user function whose name collides with a libc symbol no longer recurses into the C runtime.** A Gossamer `fn getenv(...)` emitted a global `getenv` symbol that interposed libc's `getenv`, so the runtime's `gos_rt_os_env` -> `std::env::var` -> `getenv` path recursed into the user function until the stack overflowed. User function symbols are now prefixed (`gosu.<name>`) on the LLVM tier, leaving the entry point and `[rust-bindings]` imports verbatim, so no user name can shadow a libc/runtime symbol.
- **A struct value moved into a `HashMap` keeps its `String` / `Vec` fields alive.** The stored entry aliases the inserted value's heap children (the map shares the single owning reference), but the inserting scope's drop still released those children, so a later `map.get(k).field` read dangled and `gos build` core-dumped after intervening allocation churn (the VM tolerated it). Insertion into a `HashMap` / `BTreeMap` / `HashSet` is now an ownership move: the source binding's release of the inserted value's children is suppressed, so the container holds the reference until the entry is popped (where the receiving binding releases it). Removing a release can only delay a free, never free early, so this cannot double-free; an entry that is never popped leaks its children rather than corrupting the heap. Fixtures: `map_value_heap_children.gos`, `map_pop_then_drop.gos`.
- **A struct value read back from a `HashMap` derefs its boxed storage on every access path.** A map stores a struct value as a boxed pointer; reading a field of one back out (`p.field`) has to deref that pointer rather than read the pointer bits inline. The qualified `HashMap::pop(m, k)` free-fn typed its `Option<V>` payload as a bare `i64`, so a popped struct's `p.field` lowered to the dynamic JSON accessor and faulted; `HashMap::get(m, k)` was not lowered at all (an undefined `@HashMap::get` symbol broke the LLVM build); and `for (k, v) in m.iter()` / `for v in m.values()` typed the value as `i64`, reading the pointer bits as inline fields and yielding zero. All four paths now recover the map's value type (typing the `m.values()` element as a reference so its box pointer derefs), so struct, nested-struct, and enum values read identically across the bytecode VM, Cranelift, and LLVM tiers. Fixture: `map_struct_value_access.gos`.
- **A `HashMap<String, _>` field reached as a method receiver keeps its key/value types.** Accessing a map field inline through a `Result` match-payload binding (`match mk() { Ok(m) => m.tags.get(&"a") }`) - rather than binding the field to a local first - left the receiver's type degraded (its key/value substitution lost), so `.get` dispatched to the `i64`-keyed helper (returning `None` on a string-keyed map) and `.keys` / `.values` typed their result element as `Vec<i64>`, formatting the live string keys as integers on the compiled tiers. The two MIR dispatch sites now recover the field's declared type with the struct instantiation's generic arguments applied, so inline access matches a `let`-bound receiver across the bytecode VM, Cranelift, and LLVM tiers. Fixture: `hashmap_field_through_result.gos`.
- **`Weak::upgrade` on a native enum no longer frees the node out from under a live handle.** A VM-constructed heap enum was tagged as an exclusively-owned tree, so its drop drained the node's entire strong count - including the extra count a still-live `upgrade` result holds - and a subsequent weak release then freed the node while that borrowed handle was alive (a use-after-free surfacing as intermittent heap corruption at teardown, and reliably once the `Value` enum was restored to its 16-byte budget). VM-constructed nodes now use the standard release-one / free-when-last teardown (a fresh node is cleanly reference-counted, unlike a JIT-returned tree that carries caller-cleans over-retention and still drains). Confirmed use-after-free-free under AddressSanitizer. Fixture: `weak_refs.gos` across all three tiers.
- **`xs.pop()` / `xs.first()` / `xs.last()` on a `Vec` of structs returns the whole element.** A multi-word struct is stored inline in a Vec, but these three `Option<T>` extractors loaded only the first 8-byte word of the popped element. The compiled tier then treated that single word as a pointer to the struct and dereferenced it for field access - a wild read that core-dumped when a struct carried a `String` field (an `i64`-only struct happened to survive). They now return a pointer to the element for multi-word values, matching the in-place `xs[i]` read, so `match xs.pop() { Some(p) => p.field }` reads correct fields on every tier.
- **A struct received over a channel held in a local binding reads its fields correctly.** A multi-field struct (carrying a `String`) sent over a channel and received through an inferred `let (tx, rx) = channel()` - whether `rx.recv()` in a `while let` / the same function as the send, or a `select { m = rx.recv() => ... }` arm - mis-read its fields (zero, a pointer value, or a core-dump) on the compiled tiers; it worked only when the `Receiver<T>` was a typed function parameter. Three coordinated fixes: `channel()` now types as `(Sender<?T>, Receiver<?T>)` so an inferred local channel's element unifies from `tx.send(v)`; the channel-creation lowering writes the handle into a typed tuple destination's two slots (rather than storing the pair-buffer address, which left `.0` / `.1` reading the pointer bits); and a single word stored into a multi-slot aggregate is treated as a boxed pointer and copied in full (covering `gos_rt_select_value`'s `i64` payload, with the select arm binding now typed as the channel element). Identical across the bytecode VM, Cranelift, and LLVM tiers. Fixtures: `chan_struct_local_recv.gos`, `chan_select_struct_payload.gos`.
- **`std::http_h3` is registered as a resolvable module.** `http_h3::serve` was exported without `http_h3` in the module table, failing the stdlib-export consistency check; the module is now listed.
- **`Vec::remove(xs, i)` removes the element in place on every tier.** The compiled `gos_rt_vec_remove_safe` read the element at `i` and returned it but never shifted the tail or shrank the Vec, so `xs` was left unchanged; the interpreter's builtin mutated a throwaway copy. The runtime now shifts in place and the VM threads the removal back through a dedicated `VecRemoveAt` op, so `let e = Vec::remove(xs, i)?` returns the element and `xs` loses it identically across the VM, Cranelift, and LLVM tiers. Fixture: `vec_remove_inplace.gos`.
- **`Ok((a, b))` / `Some((a, b))` carry the scrutinee's handle kind onto the tuple.** A `match listener.accept() { Ok((stream, addr)) => stream.read(..) }` left the destructured `stream` untagged on the compiled tier, so the method lowered to an undefined name-global symbol. The Ok/Some tuple-payload binding now inherits the scrutinee's runtime kind, so an `accept()` pair's stream element dispatches to the socket runtime helper.

### Features

- **Unix-domain stream sockets - `std::net::UnixListener` / `UnixStream`.** `UnixListener::bind(path)` / `accept()` / `close()` and `UnixStream::connect(path)` / `read(max)` / `read_to_string()` / `write([u8])` / `close()`, modelled on the existing TCP surface and wired across the bytecode VM, Cranelift, and LLVM tiers through a process-global handle registry. POSIX-only: the implementation is `#[cfg(unix)]`; on a non-unix target every entry point returns an `Err` (or a no-op `close`) so programs that do not use Unix sockets still build and run on Windows. Fixture: `net_unix_echo.gos`.
- **`s += &t` appends a borrowed `String` / `&str`.** The compound `+=` on a `String` now accepts a reference on the right (mirroring the `+` concatenation operator); plain `=` still requires an owned `String`.
- **The `use std::encoding::base64` / `hex` short alias is bound on the interpreter.** `base64::encode(..)` / `hex::encode(..)` resolved on the compiled tiers but not under `gos`, which only registered the fully-qualified `encoding::base64::encode` form; the short alias is now bound too.

### Optimization

- **Building a Vec with `Vec::new()` + a `push` loop is amortized O(n) on the VM.** `push` / `pop` / `insert` / `remove` now mutate the backing storage in place instead of deep-copying the whole Vec per operation, so the idiomatic `let mut v = Vec::new(); for ... { v.push(x) }` accumulation runs in amortized O(n) rather than O(n^2). Output is unchanged and identical across the VM, Cranelift, and LLVM tiers. Fixture: `vec_inplace_growth.gos`.
- **Numeric Vecs and ranges use flat 8-byte storage on the VM.** A `Vec<i64>` / `Vec<f64>` and an integer range now store one 8-byte element per slot (`IntArray` / `FloatVec`) instead of a boxed 16-byte element, halving the per-element footprint of numeric collections on the bytecode VM. A perf-only change; output is identical across every tier. Fixture: `vec_inplace_growth.gos`.

## 0.16.0 - Tier parity, aggregate formatting, optimizations, and language features

### Safety and correctness

- **A method called directly on a stdlib temporary resolves the same as a `let`-bound receiver.** `env::args().first()`, `"a,b".split(",").len()`, and similar chains left the receiver's type an inference variable, so the compiled tiers emitted an undefined bare `@method` (a hard `gos build` link failure) or mis-typed the result. Method dispatch now recovers the receiver kind from the lowered MIR type, so a chained temporary behaves identically to a bound receiver across all tiers. Fixture: `temporary_method_dispatch.gos`.
- **`for x in "...".split_whitespace()` and `for x in xs.reversed()` bind the right element type.** Iterating a `split_whitespace()` / `splitn()` result, or an element-preserving adapter (`reversed` / `to_vec` / `clone`) consumed inline, defaulted the loop element to i64 and rendered `[String]` elements as raw pointer bits under LLVM. The for-loop lowering now resolves the element type recursively from the iterable.
- **`first()` / `last()` / `pop()` over a sequence carry their `Option<elem>` payload type.** `["a","b"].first().unwrap()` and `match xs.first() { Some(s) => ... }` rendered the string pointer as an integer on the compiled tiers because the payload defaulted to i64. The checker now types Vec/Slice/Array element accessors as `Option<elem>` and the `reversed` / `to_vec` / `split` family by element, and dispatch pins the `Option` payload from the receiver's element type.
- **`[String].sort()` orders by value, not by pointer address.** `sort()` dispatched to the integer sort for every element type, so a `Vec<String>` was ordered by element pointer address on the compiled tiers. A new `gos_rt_vec_sort_str` (lexicographic UTF-8, in place) is wired across the VM, Cranelift, and LLVM tiers, and `xs.sort()` on a string vec routes to it.
- **`m.pop(k)` on a `HashMap` returns `Option<V>` and preserves the map on the VM.** The name-global `pop` resolved to the Vec pop builtin and the mutating-writeback then overwrote the map binding with that result, so the VM returned `None` and dropped every other entry. A HashMap-typed receiver now routes to the qualified map builtin and skips the Vec writeback.
- **`VecDeque<T>` tracks its element type, so `pop_front()` binds `Option<T>` with the right payload.** A `VecDeque<String>` rendered its `pop_front()` / drain payload as the i64 pointer bits on the compiled tiers because the element type was dropped at construction. `VecDeque<T>` now resolves to a sentinel Adt carrying `T` (annotated), and `VecDeque::new()` plus the first `push_back` infer `T` (unannotated); dispatch pins the `Option<T>` payload from the receiver's element. Fixture: `vecdeque_element_typing.gos`.
- **`u64` / `usize` above 2^63 compare, shift, and print as unsigned by declared type.** Every <=64-bit integer runs signed-i64 at runtime, so a `u64` past 2^63 compared and printed as negative, and the VM and LLVM tiers disagreed (`big > small` was VM `false` / LLVM `true`). Comparisons now emit `icmp ult/ule/...`, `>>` emits `lshr`, and Display picks the unsigned printer when the operand's declared type is unsigned and >= 64-bit - on both the VM and the LLVM tier, which now agree; `u8`/`u16`/`u32` and every signed type keep signed semantics. Fixture: `u64_unsigned.gos`.
- **Borrowing an array of aggregates as `&[T]` no longer double-frees its elements.** Passing `&xs` (where `xs` is a `[Tag; N]` array whose element owns a heap child, e.g. a `String`) to a `&[T]` parameter coerced the array into an owning `GosVec` that deep-freed the elements at the call's end, while the source array freed them again at scope exit - a segfault under `gos build` at teardown. A `&[T]` parameter is a borrow, so the coercion now builds a non-owning view (`gos_rt_vec_borrow_arr`) that frees only its slot-copy buffer, leaving the element children to the array that owns them. Fixture: `slice_param_coercion.gos`.
- **`Vec<String>.slice()` / `Vec::slice` / `Vec::remove` carry the element type through their `Result`.** The safe Vec helpers hard-coded their result element as `i64`, so the unwrapped sub-vec / removed value of a `Vec<String>` rendered the raw heap-pointer bits as an integer on the compiled tiers (the VM, being dynamically typed, was correct). The checker and the MIR lowering now type these results from the receiver's element. Fixture: `slice_methods.gos`.
- **A by-value struct copied into another aggregate's field no longer double-frees.** Storing a struct that owns a heap child (`Team { lead: user, .. }` where `user: User { name: String }`) and reading the source again afterward freed the shared child twice at teardown (a `gos build` segfault). The per-field retain-on-copy now fires for an `Rvalue::Aggregate` operand, not only a whole-local `Use(Copy)`, so the source and the new owner each free their own share. Covered by `record_update.gos`.
- **A field reached through a projected receiver (`t.0.x`, `pair.0.tag`) is typed by the projected receiver, not the root local.** A struct reached through a tuple element took its field type from the whole tuple instead of the projected element, so `t.0.x` was typed as element-0's struct rather than the field's `i64` - and the compiled-tier print dispatcher called the struct's `fmt` on the `i64`, dereferencing the value as a pointer (segfault). The MIR field-access lowering now walks the receiver's projection to find the real type. Fixture: `nested_field_access.gos`.

### Language and formatting

- **`{}` / `{:?}` on a struct or enum without `#[derive(Debug)]` lowers on the compiled tiers.** It previously printed on the VM but hard-errored under `gos build` with "unsupported: println/format of aggregate or variant types". A structural `fmt` is now synthesized for every struct and enum whose fields are renderable (primitives, `char`, `String`, nested structs/enums, and `Vec` of those), unless the user wrote their own `fmt` or a `#[derive(Debug)]` already requested one. Output is byte-identical to the VM. Fixture: `fmt_struct_enum.gos`.
- **`{}` / `{:?}` on tuples and `HashMap`s lowers on the compiled tiers too.** A scalar/`String` tuple renders `(1, hi, 3.5)` (and `(42,)` for a 1-tuple) via `gos_rt_tuple_format`, and a scalar/`String`-keyed map renders `{"a": 1, "b": 2}` key-sorted via `gos_rt_map_format`, byte-identical to the VM (whose map render is now key-sorted to match). Narrow-int / `f32` tuple elements and non-scalar map values keep the prior path. Fixture: `fmt_tuple_map.gos`.
- **Method dispatch resolves by the receiver's type before the bare name.** `parts.join(sep)` on a `[String]` reaches `strings::join` (it previously returned just the separator), and `String` / `Vec` collisions (`len`, `contains`) resolve by receiver kind across every tier. Fixture: `method_dispatch_collisions.gos`.
- **Slice / rest patterns.** `match xs { [first, ..rest] => ..., [a, b] => ..., [] => ... }` desugars to a length guard plus indexed element binds at HIR construction, so it runs identically on all three tiers. Fixture: `slice_patterns.gos`. (`..rest` capture is `i64`-element only on the compiled tier pending a `Vec<String>` slice-helper fix.)
- **Labeled loops.** `'outer: loop { ... break 'outer value }` / `continue 'outer` on `loop` / `for` / `while`, so nested-loop early exit no longer needs a sentinel flag. The lexer reads `'ident` not closed by a quote as a label; break/continue target the named loop. Fixture: `labeled_loops.gos`.
- **Functional record update `Type { ..base, field: value }`.** A struct literal may spread a base value and override named fields; the spread reads in any position (`{ ..base, x: 1 }` or `{ x: 1, ..base }`), explicit fields win over the base for the same name, and a second spread is a parse error. A field filled from `..base` shares the base's heap children (strings, vecs, nested nodes), so the new struct retains its own share - the base keeps reading and reclaims at its own drop, with no double-free. Output is byte-identical across every tier. Fixture: `record_update.gos`.
- **Generic functions with trait bounds, dispatched statically.** `fn report<T: Shape>(s: &T) -> String { format!("{}: {}", s.name(), s.area()) }` is now callable with any number of concrete types in one program; each call instantiates the parameters independently, the bound is enforced (a type with no `impl Shape` is a `GT0017` compile error), and a method on the bound parameter (`s.name()`) resolves to the trait method's declared return type. Each instantiation is monomorphised and the trait-method call is rewritten to the concrete impl symbol (`Square::name`), so it links and runs bit-identically across the VM, Cranelift, and LLVM tiers. Scope: single-bound type parameters with struct arguments and inherent static dispatch; `dyn Trait`, operator traits, associated types, blanket impls, and supertraits remain out of scope. Fixture: `trait_bounds.gos`.

### Optimization

- **A String `+` chain allocates once.** `a + b + c + ...` (three or more operands) folds into the same n-ary single-pass concat that `format!` uses, instead of one intermediate `String` per operator. A two-operand `+` keeps the existing path; output is byte-identical across every tier. Fixture: `string_concat_chain.gos`.
- **The MIR inliner refuses self-recursive callees.** Multi-block and call-containing callees already inline (`inline_general`); a self-recursive callee is no longer registered as inlinable, so recursion stays a real call instead of being spliced one level per pass up to the growth budget.
- **Bounds-check elision on provably in-range index loops.** A MIR pass proves a `for i in 0..xs.len()` loop keeps `i` in range and `xs` unmutated, then rewrites `xs[i]` reads/writes to a guard-free `gos_rt_vec_get/set_i64_unchecked` (scalar elements only), letting the LLVM `opt -O3` pipeline vectorise the inner loop. Conservative and bail-closed (inclusive ranges, len arithmetic, mutated/aliased collections keep the check). Cranelift maps the unchecked symbol back to the safe one, so tier output is unchanged. Fixture: `bounds_check_elim.gos`.
- **JIT now compiles aggregate-param and closure-taking functions.** The in-process Cranelift JIT (`gos`) skipped any function taking or returning a `Vec` / `String`, dropping idiomatic `sort_by` / `map` / `fold` code to the bytecode VM. It now marshals pointer-shaped aggregates across the JIT boundary (caller-owned, freed by the trampoline through the runtime RC reclaim entries, with write-back for in-place mutators), so such functions run natively. Fixture: `jit_aggregate_param.gos`.

### Tooling

- **REPL tab-completion.** Tab completes the identifier or `module::item` path at the cursor against the keyword set and the standard-library surface (module paths and their members, in both `std::strings` and bare `strings` forms).
- **`[rust-bindings]` may now depend on `gossamer-*` from crates.io or git.** A binding that pulled `gossamer-runtime` / `gossamer-std` / `gossamer-binding` from a non-path source linked a second copy of `gossamer-runtime` beside the toolchain's own, and since `gossamer-runtime` owns the process `#[global_allocator]`, `gos build` failed to link. The generated build-root manifest (the runner and the compiled-mode staticlib) now emits a `[patch]` block redirecting each binding's `gossamer-*` crates to the toolchain checkout, so cargo unifies them to one source. The patch key follows the binding's declared source - `[patch.crates-io]` for version deps, `[patch."<git-url>"]` for git deps - detected from the binding crate's `Cargo.toml` when it is on disk, defaulting to crates.io for crates.io / git bindings. Contract: a binding declares a crates.io `gossamer-* = "<req>"` requirement (a path dependency cannot be redirected and breaks once the binding is fetched from git). The `[patch]` supplies the toolchain checkout, so any requirement the toolchain version satisfies works - `gossamer-* = ">=0.16.0"` is recommended over an exact `= "=0.16.0"` so a binding survives `gos` upgrades instead of needing a re-pin each release.

## 0.15.0 - Safety, performance, and language expressiveness

### Safety and correctness

- **`*out += format!(...)` onto a `&mut String` accumulator no longer corrupts on the compiled tiers.** The in-place deref-append fusion bailed to the general `*out + piece` lowering whenever a format piece had an unresolved type (e.g. an enum-payload binding the checker left as an inference variable, as in `match v { Node::Leaf(n) => *out += format!("{}", n) }`). That general path read the `&mut String` slot pointer as string bytes, producing garbage and a use-after-free in recursive serializers threading a `&mut String` - the canonical hand-rolled-serializer shape. The fusion now appends the whole formatted result in place via `gos_rt_str_concat_drop_a` instead of bailing, keeping the accumulator on the correct in-place path. The VM was unaffected; this closes a VM/compiled divergence. Fixture: `feature-testing-examples/deref_string_concat.gos`.
- **A derived `clone` no longer hijacks `.clone()` on `String` / `Vec` / enum receivers.** Once any struct carried `#[derive(Clone)]`, a `.clone()` on a `String` or `Vec` receiver could dispatch to that struct's synthesized `clone` (a `GX0001` on the VM). `String` / `Vec` receivers now resolve to the universal builtin clone by type key, ahead of the bare-name fallback, so a derived `clone` never changes the outcome for a built-in receiver. Fixture: `clone_builtin_dispatch.gos`.
- **A mutating builtin on a nested place persists on the VM.** `groups[i].push(x)` (a `push` reached through an index) and `bag.items.push(x)` (through a struct field) were silently lost on `gos` while the compiled tiers mutated the backing storage in place. The VM now splices the returned aggregate back through the place-store protocol, so nested-vec and struct-field mutation - the shape group-by aggregation relies on - agrees across tiers. Fixture: `nested_vec_mutation.gos`.
- **A single-field struct stored in a `Vec` addresses its field correctly.** When a struct's only field is a `Vec` (slot size 8), `buckets[i].items` strided off the `GosVec` header instead of the element's data buffer, corrupting reads and in-place pushes through the indexed field. The element address now resolves inside the data buffer. Fixture: `vec_single_field_struct.gos`.
- **Weak reference upgrade is now data-race-free.** `gos_rt_rc_weak_upgrade` previously used a plain non-atomic read-modify-write on the strong count, bypassing `SHARED_BIT`. Two goroutines simultaneously upgrading the same weak reference could tear the count and produce a use-after-free. The upgrade now uses a CAS loop via `inc_strong`, matching the atomic path all other retain/release operations take for shared objects.
- **`RcHeader.weak` is now an `AtomicU8`.** The weak count was a plain `u8` with non-atomic increments/decrements in `gos_rt_rc_downgrade`, `gos_rt_rc_weak_retain`, and `gos_rt_rc_weak_release`. Concurrent weak operations on shared objects could tear the count. All three functions now dispatch on `SHARED_BIT` and use atomic read-modify-write for shared objects.
- **Cycle collector visits shared children atomically.** `mark_gray` and `scan_black` called `set_strong_count` (plain non-atomic write) on every RC child without checking `SHARED_BIT`. A non-shared struct can reference a shared child (sent to another goroutine); the cycle collector running on one goroutine now uses `fetch_sub(1, Acquire)` for shared children, matching the regular release path.
- **Cycle collector releases string children of collected garbage.** `collect_white`/`free_block` used the filtered `visit_rc_children` which skips strings. String fields in structs participating in a cycle were permanently leaked. The collector now uses `visit_children_raw` and calls `gos_rt_str_drop` on string fields before freeing the block.
- **`GosVec` embedded reference count is now atomic.** The `_reserved[1..3]` embedded refcount was a plain `u16` with non-atomic read-modify-write. Concurrent access from two goroutines holding the last two references could produce a double-free or permanent leak. The field is now `AtomicU16` with `fetch_sub(1, Relaxed)` and return-value check.
- **`HashMap.clear()` no longer leaks RC blob values.** Previously replaced the storage with `MapStorage::Empty`, dropping `i64` values as plain integers without calling `release_blob_value`. Blob-valued maps (e.g. `HashMap<String, Vec<T>>`) now walk and release all values before clearing.
- **`HashMap.remove()` on string-keyed blob maps no longer leaks.** `gos_rt_map_remove_str` was missing the `blob_values` check and `release_blob_value` call that `gos_rt_map_remove_i64` already had. Fixed to match the i64 path.
- **VM channels now enforce capacity.** All VM channels were unbounded regardless of the capacity argument, causing divergence with the compiled tier where `channel(N)` enforces a backpressure limit. Bounded channels now park the sending goroutine when full, matching compiled-tier behavior.
- **The VM HTTP `ResponseStream` handle registry no longer recycles slots.** Stream handles are now allocated monotonically, so a stale stream value (one already consumed by `Response::stream(...)`) looks up an absent handle and yields `None` rather than colliding with a later stream that reused its slot - matching the compiled tier's `NEXT_STREAM_HANDLE`.
- **`fs::remove_dir` and `os::remove_dir` have consistent semantics across tiers.** The VM called `std::fs::remove_dir` (non-recursive) while the compiled tier dispatched to `remove_dir_all` (recursive). Both now use the same non-recursive operation; use `fs::remove_dir_all` for recursive removal.
- **Seven stdlib functions that worked under `gos` but silently failed under `gos build` are now wired across all three tiers**, each with a tier-parity fixture: `encoding::yaml::encode`, `encoding::yaml::parse_all`, `encoding::json::encode_pretty`, `fs::create_dir`, `path::split`, `encoding::base32::decode`, `encoding::base32::decode_hex`.

### Performance

- **String-append fusion through a `&mut String`.** A string-literal fragment appended onto a `*out` accumulator lowers to `gos_rt_str_append_bytes` (length-counted, no per-call `strlen`), which the LLVM tier inlines to a capacity check + `memcpy` + length bump - no FFI call on the in-place path; `*out += format!("{}", n)` fuses to `gos_rt_str_append_i64` / `_f64` written straight into the pointed-to string, eliminating the per-value intermediate allocation. The three append shims (`gos_rt_str_concat_drop_a`, `_append_i64`, `_append_f64`) drop their `catch_unwind` (`ffi_entry!`) wrapper - they are panic-free across the boundary - and the append result stays on the self-consuming copy-back path (no spurious retain/release), so the builder's refcount stays 1 and appends remain in place rather than degrading to copy-on-write reallocation. Fixtures: `string_append_realloc.gos`, `deref_string_concat.gos`.
- **Array indexing inside small inlined callees uses the right index.** `inline_small_callees` remapped a spliced callee's place root but not its `Projection::Index` local, so a small callee like `fn at(a: &[T; N], i) -> a[i]`, once inlined, read the index through a colliding caller local - a latent miscompile that surfaced as an out-of-bounds panic (garbage index) in iron_knight's `gen_pawn_moves` (`pos.boards[us][PAWN]`). The index local is now remapped like the place root. The VM was correct; this closes a compiled-tier divergence. Fixture: `inline_index_remap.gos`.
- **`[bool]` element stride is now 1 byte.** Bool arrays previously used 8-byte element stride, making a 1M-element `[bool]` consume 8 MB instead of 1 MB. Random-access workloads (BFS `visited` arrays, presence tracking) were spending most of their time in DRAM rather than L3. With 1-byte stride, the visited array for a 1M-node graph-BFS fits in L2 cache. The push/get/set inline paths in the LLVM codegen have a corresponding byte-stride fast path.
- **`HashSet` operations no longer allocate per lookup.** `contains`, `insert`, and `remove` were allocating a `String` for each operation by converting the raw key to an owned string before hashing. The backing `HashSet` now uses hashbrown's borrowed-key API so lookups hash and compare the raw bytes directly.
- **`PRIMITIVE` vec free skips lock acquisition.** `vec_elem_meta_remove` was acquiring `VEC_ELEM_METAS` and `VEC_SLOT_CHILDREN` locks unconditionally even for primitive-element vecs with no entries in either table. A PRIMITIVE early exit avoids both mutex acquires for the common case.
- **Inliner cost limit raised from 24 to 40; constant-argument promotion added.** The recursive-descent JSON parser's `parse_val`, `parse_str`, and `parse_num` helpers were previously too large to inline. With the raised limit and a constant-argument promotion path (callees over the limit are inlined when at least one argument is a compile-time constant, since `const_fold` immediately collapses branches and reduces effective post-inline size), the full parser inlines into the dispatch loop and LLVM can optimize the parse-then-access pattern end to end.
- **Dead basic blocks are swept after `const_branch_elim`.** Constant branch folding converts `SwitchInt` to `Goto` but left orphaned unreachable arms in the CFG, contributing to IR size and slowing LLVM's middle-end. A reachability sweep now follows `const_branch_elim` and drops all unreachable blocks before the IR is handed to LLVM.

### Language - expressiveness

- **`#[derive(Clone)]` works on enums with struct-payload variants.** Struct-payload variants (`Shape::Rect { w: f64, h: f64 }`) previously required hand-written clone functions. The derive synthesiser now covers named-field variants using the same mechanism as tuple variants (already working). Eliminates hundreds of lines of boilerplate in programs with rich enum hierarchies.
- **String literal patterns in `match`.** `match s { "SELECT" => ..., "FROM" => ..., _ => ... }` desugars to a chain of equality tests at HIR construction time. Eliminates the separate lookup-table helper functions that were previously required for string dispatch.
- **Or-patterns in match arms.** `Variant1(x) | Variant2(x) => body` shares a single arm body for multiple patterns. Both alternatives must bind the same names. Exhaustiveness analysis accounts for or-patterns as a single combined case.
- **`String::push_char(c)` and `String::push_byte(b)`.** In-place single-character and single-byte append with no intermediate allocation. Serializers that previously used `s += "\""` (which allocates a temporary `String`) now use `s.push_char('"')`. Wired across VM, Cranelift, and LLVM.
- **`std::collections::VecDeque<T>`.** A ring-buffer FIFO queue. `push_back`, `pop_front`, `len`, `is_empty`, and iteration. Wired across all three tiers with a tier-parity fixture.
- **Runtime-sized filled arrays: `[val; n]` with a variable `n`.** A non-constant repeat count now types the literal as a heap `Vec<elem>` (a compile-time count stays a fixed `[T; N]`) and lowers through the vec path, so `[0; n]` / `[false; n]` no longer require an explicit push loop. Fixture: `vec_runtime_repeat.gos`.
- **`for i in 0u32..n` (range bounds accept any integer type).** Loop variables and range bounds are no longer restricted to `i64`. Non-`i64` bounds are widened in the for-loop desugaring; the bound type does not propagate into the body.
- **Tuple patterns in `match`: `match (a, b) { (Variant1(x), Variant2(y)) => }`.** Desugared to nested match at HIR time. Eliminates the O(m * n) code duplication that multi-dimensional enum dispatch previously required.
- **Entry files may omit the `fn main` wrapper.** Bare top-level statements become the body of an implicit `fn main()`, with items (`fn`, `struct`, `enum`, `const`, `static`, `use`, `mod`) hoisted out. A `?` in top-level code makes the implicit main return `Result<(), errors::Error>`; set a process exit code with `std::process::exit(n)`. The optional `[project] entry` manifest key selects the entry source, overriding convention-based resolution. A pure front-end desugar into an ordinary `fn main`, so it works identically across the bytecode VM, Cranelift JIT, and LLVM AOT tiers.

## 0.14.1 - Dependency audit (cleanup, upgrades, unmaintained-crate replacement)

- **Removed unused dependencies.** Dropped 12 unused direct deps (`csv`, `crossbeam-utils`, `serde`, `futures-core`, `futures-task`, `pin-project-lite` from `gossamer-runtime`; `csv`, `libloading`, `futures-core`, `futures-task`, `pin-project-lite` from `gossamer-std`; `gossamer-runtime` from `gossamer-codegen-llvm`) plus the dead `codespan-reporting` / `gimli` workspace declarations.
- **Replaced unmaintained/deprecated crates.** `serde_yaml` (archived) -> `serde_norway`; `fs2` (unmaintained) -> `fs4`; the cache codec moved off `bincode` -> `postcard`, which drops the `bincode` `RUSTSEC-2025-0141` advisory entirely (the whole crate is now unmaintained, not just 1.x).
- **Dependency upgrades.** cranelift 0.123 -> 0.132, zip 2 -> 8, notify 6 -> 8, rustyline 14 -> 18, toml 0.8 -> 0.9, quick-xml 0.37 -> 0.40, x509-parser 0.16 -> 0.18, nix 0.29 -> 0.31, windows-sys 0.59 -> 0.61, getrandom 0.2 -> 0.3, rcgen 0.13 -> 0.14, plus `which` / `corosensei` / `object` patches.
- **Collapsed duplicate versions.** The upgrades drop the second copies of `mio` (0.8), `thiserror` (1.x), `nix` (0.28), and `bincode` (1.x) from the tree.
- **API migrations are behavior-preserving.** `quick-xml` 0.40 now reports entity references as their own `GeneralRef` events; the XML codec reassembles and entity-resolves character data per element so output stays byte-identical across the VM and compiled tiers. 

## 0.14.0 - Tree-walker removed (VM-only execution), standard-library ergonomics, stability, performance, parity, _.method pipe placeholder, deep technical debt

- **The tree-walking interpreter is gone - the register-based bytecode VM lowers every construct natively.** `gos` / `gos test` / `gos bench` and the REPL previously fell back to a bundled tree-walker (via `Op::EvalDeferred`) for closures, `select`, `defer`, non-call `go { block }`, or-patterns that bind a variable, custom (`impl Iterator`) for-loops, and a handful of other shapes - and `gos test` / `gos bench` / the REPL ran *entirely* on the walker. The VM now compiles all of these to native bytecode: closures lower to `Op::MakeClosure` with upvalue capture (snapshot scalars, `Arc`-shared aggregates), `select` to a native channel poll/park (`Op::Select`), `defer` to LIFO emission at every exit edge (fall-through / `return` / `break` / `continue` / the `?` path), or-patterns to shared binding registers, `go { block }` to a spawned closure, and struct/enum `==` and nested `a.b.c = x` assignment to direct opcodes. En route, two pre-existing VM correctness gaps were fixed across all tiers: `&mut self` struct-method mutation now persists (`c.bump()` was silently lost on `gos`), and `static mut` storage is now shared and observable (writes were silently dropped on *every* tier, masked by tier-parity agreeing on the wrong answer). `Op::EvalDeferred`, `compile_deferred`, and the ~2,900-line walker (`interp.rs` + `env.rs`) are deleted; the bytecode VM is the single `gos` / `gos test` / `gos bench` / REPL engine, its output pinned by the VM-vs-LLVM-AOT differential and the tier-parity suite. `gos test` additionally gains statement-level coverage instrumentation and a call-chain traceback on a failed test.
- **Format-spec mini-language.** `{}` placeholders now accept the Rust-style spec grammar: width and alignment (`{:>8}` / `{:<8}` / `{:^8}` / `{:8}`), fill characters (`{:*>8}`), zero-padding (`{:08}`), radix (`{:x}` / `{:X}` / `{:b}` / `{:o}`), and precision combined with width (`{:>8.2}`) - for positional and named (`{n:03}`) arguments. Each spec expands at parse time to a composition of `__concat` / `__fmt_radix` / `__fmt_prec` / `__fmt_pad`, so it runs bit-identically on the VM, Cranelift, and LLVM. Previously only `{}`, `{ident}`, `{:?}`, and `{:.N}` were recognized; richer specs fell through as literal text.
- **HashSet algebra.** `union`, `intersection`, `difference`, `symmetric_difference` (returning a fresh set) and the `is_subset` / `is_superset` / `is_disjoint` predicates, wired across every tier. A `HashSet<T>` (and `BTreeMap<K, V>`) annotation now resolves to a named type, so method dispatch works when a set flows across a function boundary - previously a returned/parameter set lost its construction tag and `s.contains(...)` emitted an undefined symbol on the compiled tiers.
- **`strconv` radix + quoting.** `parse_i64_radix(s, base)` / `format_i64_radix(n, base)` for bases 2..=36, and `quote(s)` / `unquote(s)` for round-tripping double-quoted strings with escapes.
- **Channel-returning timer.** `time::after(d)` returns a `Receiver` that yields once after `d`, firing on a goroutine that then completes, so it composes with `select` and `while let` for timeout patterns. `channel()` is now a prelude name (like `spawn`).
- **String surface naming - one spelling per operation.** `index_any` / `last_index_any` renamed to `find_any` / `rfind_any` (symmetric with `find` / `rfind`); `lstrip_chars` / `rstrip_chars` renamed to `trim_start_matches` / `trim_end_matches` (the Rust `trim_*_matches` family). The duplicate spellings `to_lowercase` / `to_uppercase` (use `to_lower` / `to_upper`), `strip_chars` (use `trim_matches`), `fields` (use `split_whitespace`), and `zfill` (use `pad_left` / `{:0N}`) were removed. A method that does not exist is now a hard error on every tier - the tree-walking interpreter previously returned the receiver unchanged for an unknown method instead of erroring like the bytecode VM and compiled tiers.
- **Vec methods on array literals.** A `[T; N]` array-literal receiver is coerced to a heap `GosVec` before `contains` / `index_of` / `count_of` / `first` / `last` / `reversed` / `join`, fixing a native-tier segfault where the flat stack buffer was read through a `GosVec` header.
- **`_.method` pipe placeholder.** `x |> _.trim`, `x |> _.replace(a, b)`, `x |> _.0`, `x |> _[i]`, and `x |> _` thread the piped value in as the receiver - `_` reads as "the value flowing through here". A bare `_.ident` with no parens is a nullary method call (`s |> _.trim` is `s.trim()`); tuple/index forms stay field/index access. A pure parse-time desugar, so it runs bit-identically on the bytecode VM, Cranelift, and LLVM; the existing `x |> recv.m(a)` → `recv.m(a, x)` and closure-step forms are unchanged.
- **The canonical String method surface dispatches on every tier.** `split_whitespace`, `splitn`, `to_title`, `trim_matches`, `replacen`, `pad_left`, `pad_right`, `contains_any`, `contains_rune`, `equal_fold`, `index_any`, `index_rune`, `last_index_any`, `strip_prefix`, and `strip_suffix` previously worked only as `strings::*` free functions and emitted an undefined `@method` symbol when called as `s.method(...)` on the compiled tiers. The method form now lowers to the same `gos_rt_str_*` shim the free function uses, with the destination pinned to the correct return type (`String` / `Vec<String>` / `bool` / `Option<i64>` / `Option<String>`).
- **`Vec<String>.join(sep)` is the method form of `strings::join`.** Wired across the VM and the compiled tiers for `Vec` / `&[String]` receivers.
- **Receiver-typed method dispatch on the VM.** A method whose bare name collides with another module's free function now resolves by receiver type: `s.to_title()` reaches the string title-caser instead of `unicode::to_title` (which titlecases a single char), and `parts.join(sep)` reaches `strings::join` instead of `path::join`. String methods register under a `String::` key and `Vec`-receiver dispatch under a `Vec::` key, ahead of the bare-name fallback.
- **Fixtures**: `feature-testing-examples/pipe_placeholder.gos` and `string_method_surface.gos` cover the placeholder forms and the full wired method surface, bit-identical across VM / Cranelift / LLVM.

### Stability, performance, and parity hardening

- **Integer divide / modulo by zero is a clean panic on every tier.** The compiled tiers lowered `/` and `%` to a raw `sdiv`/`srem`, so a zero divisor trapped with `SIGFPE` (exit 136) - or, for a constant zero the folder declined to fold, produced a silently-undefined value. MIR now emits the (previously dead) `Assert{DivideByZero}` terminator before integer division, so `gos build` raises the same `error[GX0005]: panic: divide by zero` the VM does. The VM's divide-by-zero, formerly a recoverable `GX0004`, is now also a panic, so all three tiers match (code, message, and exit 101). Constant non-`0`/`-1` divisors fold the guard away, preserving the `x / 1 → x` strength reduction.
- **`i64::MIN / -1` (and `% -1`) wraps to `MIN` / `0` on every tier** instead of trapping (`SIGFPE` on x86) on the compiled tiers - matching the VM's `wrapping_div`. The guard is branchless-foldable for constant divisors.
- **`I64Vec` / `U8Vec` out-of-range `get_at` / `set_at` no longer corrupt the heap on LLVM.** The inline fast path skipped the bounds check the runtime shim and Cranelift both have, so an out-of-range index read or wrote arbitrary heap memory. It now null- and bounds-checks like the bare-`Vec` inlines (out-of-range get → 0, set → no-op).
- **`String.substring(a, b)` dispatches to the clamping byte-slice builtin on every tier.** The compiled tier had a runtime shim but no method-lowering entry, so `s.substring(0, 5)` either failed to lower or returned the pointer as garbage; the VM had no registration at all. Now wired end-to-end (clamping, infallible - the counterpart to the `Result`-returning `slice`).
- **Closing an already-closed channel panics with `close of closed channel`, matching Go.** A double-close is a goroutine-scoped panic - fatal on the main goroutine, isolated to the offending goroutine otherwise - on the VM and the compiled runtime alike, rather than aborting the whole process (which defeated goroutine-panic isolation) or silently ignoring the second close. The channel's drop/reclamation path stays idempotent, so a channel closed once and then reclaimed at end of scope does not panic.
- **Deep recursion in goroutine / closure / method bodies raises a clean `GX0008` stack-overflow instead of crashing.** Walker-evaluated recursion (spawned-goroutine bodies, closures, `impl` methods) ran unguarded on the native stack and overflowed it - fatal on the 1 MiB goroutine stack. A byte-budget stack guard (armed at each goroutine's and the main thread's shallowest frame, re-armed across goroutine migration) trips before the guard page and surfaces a normal stack-overflow error. VM-tier goroutine OS threads are now sized to the goroutine stack contract.
- **Interpreter dispatch speed.** The hottest `gos` ops (`FieldGet`, method calls, free calls) re-hashed the receiver's already-interned type name through a second thread-local pool on every execution to compute their inline-cache token; they now use the globally-interned `&'static str` pointer directly. Cached free-builtin calls no longer allocate (and free) an `Arc<BuiltinInner>` per call - the fn pointer is invoked directly and the pooled argument buffer is returned to the pool.
- **Bytecode-VM numeric loop dispatch.** Reading or writing a float field of a fixed-size struct array through an integer loop index (`bodies[i].vx`) now sources the index straight from the integer register file via dedicated `FlatGetF64I` / `FlatSetF64I` ops, dropping the per-access box of the index into a value register that every flat field access previously emitted. Separately, a bare assignment statement no longer materializes the unused `()` value it evaluates to (a `LoadConst(Unit)` per statement) - the store is compiled directly when its result is discarded, while assignment-as-expression positions still produce the unit. Both cut the instruction count of tight numeric and mutation-heavy loops on `gos`.
- **In-place string accumulation for `acc += format!(...)`.** The accumulation now appends each interpolated piece directly onto the accumulator - one copy into the growing buffer - instead of assembling the fragment in a scratch buffer, allocating a result string, and copying that into the accumulator (three copies of every character). Integer and float interpolations format their digits straight into the destination via new in-place `gos_rt_str_append_*` shims, with no throwaway string per value. A compiled-tier lowering; the produced text is bit-identical to the buffered concat path.
- **Goroutine-shared reference counts are atomic on the compiled tiers (atomic-on-escape).** A heap-RC object (recursive enum / boxed payload) shared between goroutines - captured by a `spawn` closure, passed to `go f(...)`, or sent on a channel - was retained/released with non-atomic counts under the multi-threaded scheduler, so two workers releasing it concurrently could tear the count into a use-after-free or a leak. Objects now switch to atomic reference counting when they escape to another goroutine (a `SHARED_BIT` set transitively at the escape point) and are excluded from the per-thread cycle collector (their cycles leak like Rust's `Arc` - break with weak refs). Thread-local objects keep the cheap non-atomic path, so single-goroutine RC performance is unchanged.
- **A `Vec` of by-value aggregates owns its elements' RC children on the compiled tiers.** A `Vec<T>` whose element `T` is a struct/tuple carrying an unconditional RC field - a heap user enum, `String`, or nested vec (e.g. `Vec<Projection>` where `Projection { expr: Expr, alias: String }`) - was shallow-freed while the source temporary's fields were released at scope end, so a vec built in one function and returned (it outlives the temp) left dangling pointers, and walking it after the return was a use-after-free (it corrupted the `atlas_db` SQL benchmark's query planner). The vec is now tagged `AGGR_OWNED` with a slot-children layout (`gos_rt_vec_set_slot_children`): push retains each element's RC children and free deep-frees them, so a pushed element dropped at its source scope - or carried out inside the returned vec - is reclaimed exactly once. A `for x in &v` loop variable bound to a borrowed element is no longer treated as an owning aggregate, so it no longer double-releases the element's fields.
- **The loop-carried-release hoist no longer frees a value still live on a sibling branch.** The hoist relocated a string's release to its last mention along a single back-edge path, but a `for` loop with one branch reading the value and another pushing it (a group-by accumulation: `for k in &keys { if k == key … }` then `keys.push(key)`) left the value live past that mention; nulling it there collapsed every later group key to empty on the compiled tiers. The hoist now runs a forward-liveness check from the insertion point and skips the relocation when the value is read before being rewritten on any path.
- **`assert(cond[, msg])` and `assert_eq(a, b[, msg])` are implemented on every tier.** Both were reserved prelude names with no implementation, so a call raised `error[GX0002]: name 'assert' is not bound` even though the skill card and test examples use them. They now panic on a false condition (the supplied message verbatim, else "assertion failed") via a `builtin_assert` in the interpreter and a conditional-`panic` MIR lowering for the compiled tiers; a passing `assert` is counted in the test tally.
- **`gos test` links sibling modules into the test compilation.** A `#[test]` calling a sibling module (`super::helper::triple` where `src/helper.gos` is declared `mod helper;`) failed with `GX0002` because the per-file test build read the entry source without the sibling auto-bundle that `gos` / `gos build` apply. Tests now bundle siblings the same way; test-name collection stays unbundled so a sibling's own tests are not double-counted.
- **`*p = v` through a `&mut <scalar>` parameter writes back on the interpreter.** Assigning through a scalar `&mut i64` / `&mut f64` / `&mut bool` reference raised `error[GX0007]: assignment to non-local place` on the VM (the compiled tiers, passing by pointer, wrote back correctly). The write-back cell protocol - previously `&mut Vec` / `&mut [T]` only - now covers scalar primitives and a dereferenced lvalue, so a deref-assign reaches the caller bit-identically with the compiled tiers.
- **Deterministic, cross-tier HashMap / HashSet iteration order.** `m.keys()`, `m.values()`, `for (k, v) in m.iter()`, and `set.to_vec()` now traverse in key-sorted order on every tier instead of unspecified hash-bucket order that differed across tiers (and, for `HashSet`, run-to-run). `keys()` / `values()` / `iter()` agree on ordering so positional pairing is stable.
- **`as usize` / `as u64` cast results work as comparison and arithmetic operands on the interpreter.** `(x as usize) < (y as usize)` type-errored on the VM (a `Uint` reaching the typed-i64 register path); the unbox now accepts `Uint` (every ≤64-bit integer shares i64 arithmetic), matching the compiled tiers.
- **The runtime symbol registry is sorted.** Out-of-order additions (`set_*` algebra, `fmt_pad`, `strconv` radix/quote, `str_append_*`) broke the alphabetical-order invariant the binary-search lookup relies on; the registry is re-sorted.
- **`String.len()` is the byte length on every tier.** The interpreter counted Unicode codepoints while the compiled tiers (and the `gos_rt_str_len` shim) counted bytes, so any multibyte string silently diverged (`"héllo".len()` was 5 on `gos`, 6 on `gos build`). The VM now returns bytes, matching Rust/Go; codepoint counts stay at `utf8::rune_count_in_string` / `unicode::grapheme_count`.
- **Cross-goroutine sync primitives share state on the interpreter.** The atomic / mutex / once / `sync::Map` registries were thread-local, so a handle created on one goroutine resolved to nothing on another scheduler worker thread and every update silently no-op'd (a `fetch_add` loop across goroutines counted 0 on `gos`, 1600 on `gos build`). The registries are now process-global behind a reentrant-lock wrapper, so all tiers agree; single-threaded borrow semantics are unchanged.
- **`encoding::json::valid`, qualified `math::min` / `max` / `clamp`, and `bufio::read_lines` lower on the compiled tiers.** Each had an interpreter builtin but no runtime shim or dispatch entry, so `gos build` failed with an undefined-symbol error. `json::valid` gains a `gos_rt_json_valid` shim, the `math::`-qualified scalar-cmp forms route to the existing `gos_rt_min/max/clamp_*` shims, and `bufio::read_lines` aliases `read_lines_of`.
- **`encoding::ascii85::encode` of a `String` no longer segfaults on the compiled tiers.** A `String` argument was missing from the byte-coercion whitelist, so the c-string pointer was read as a `GosVec` header; `ascii85::encode` joins `base64` / `hex` / `base32` in the whitelist.
- **`utf8::rune_len(<codepoint>)` returns the byte length on the interpreter.** The VM builtin only handled a `char` receiver and returned 0 for an integer codepoint; it now accepts the scalar and matches the compiled `gos_rt_utf8_rune_len` (invalid scalar → -1).
- **Router path parameters reach Gossamer handlers.** `r.path_value(name) -> String` returns a router-captured `{id}` / `{rest...}` segment (Go's `PathValue` semantics; `""` when the matched pattern declares no such capture), wired across the VM and compiled tiers. The VM router now also supports `{rest...}` trailing captures, matching the compiled matcher.
- **Typed router path extractors.** `r.path_int(name) -> Option<i64>` and `r.path_float(name) -> Option<f64>` parse a captured segment to a typed value (`None` when absent or unparseable), the Gossamer analog of Rust's typed `Path<T>` extractor. Wired across the VM and compiled tiers over the packed-`Option<T>` ABI.
- **`std::compress::zstd` works on every tier.** `encode` / `encode_level(1..=22)` / `decode` were manifest-advertised but unbound; they now have interp builtins and `gos_rt_compress_zstd_*` runtime shims, mirroring the gzip/flate/zlib wiring (a `String` argument is byte-coerced before the shim, like the other encoders).
- **`crypto::password` (Argon2id) works on every tier.** `hash` / `verify` / `needs_rehash` were manifest-advertised but unbound; now wired with `gos_rt_crypto_password_*` shims using the same Argon2id defaults as `kdf::argon2id` (Argon2id, V0x13, default params), so a PHC hash minted on one tier verifies on another. `hash` returns `Result<String, Error>`, `verify` returns `Result<bool, Error>`.

### Stdlib breadth - modules wired across all three tiers

Many modules and methods had an interpreter implementation but no compiled-tier shim (BUILD-FAIL under `gos build`) or were manifest-advertised but unbound everywhere (`GX0002`). The following now run bit-identically on the bytecode VM, Cranelift JIT, and LLVM AOT, each with a tier-parity fixture:

- **Crypto breadth.** `crypto::insecure::md5_hex`/`sha1_hex`, `crypto::kdf::pbkdf2_sha256`/`argon2id_hash`/`argon2id_verify`/`scrypt_interactive`, `crypto::aead::aes_256_gcm_seal`/`open` + `chacha20_poly1305_seal`/`open`, `crypto::ed25519::keypair`/`sign`/`verify`, and `crypto::ecdsa::keypair_pem`/`sign_pem`/`verify_pem` now have compiled shims reimplementing the same crates the interpreter uses (the runtime cannot depend on `gossamer-std`).
- **`std::jwt`.** Sign/verify for HS256/384/512, ES256, and EdDSA, via a JSON-string claims API (`sign_hs`/`verify_hs` take/return the claims as a JSON object string), avoiding a `Claims` struct crossing the C-ABI.
- **`std::compress::bzip2`** (`compress`/`decompress`).
- **`encoding::xml`** (`parse`/`encode` over an opaque node handle) and **`encoding::yaml::parse`** (compiled phantom - now projects YAML→JSON like the VM).
- **Phantom stateful-handle modules.** `std::math::rand` (`Rng` SplitMix64), `std::bytes` (`Builder`/`Buffer`), `std::validate` (`FieldError`/`Errors`), `sync::RwLock`, `std::context`, `std::metrics` (`Counter`/`Gauge`/`Histogram`/`Registry`), and `std::trace` (`Tracer`/`Span`/`EndedSpan`) are wired via the opaque-handle pattern; closure-taking methods (`RwLock::with_read`/`with_write`, `Once::call`) cross the FFI via the env-thunk convention.
- **Networking - TCP / UDP / `net::ip`.** `TcpListener::bind`/`accept`/`local_addr`/`close`, `TcpStream::connect`/`read`/`read_to_string`/`write`/`close`, `UdpSocket::bind`/`send_to`/`recv_from`/`local_addr`/`close`, and `net::ip::parse`/`is_valid`/`is_v4`/`is_v6`/`is_loopback`/`octets` now compile (`std::net`, cross-platform). A captured `TcpStream` handle from `accept()`'s tuple dispatches its methods on the compiled tier.
- **`iter` combinators.** `enumerate`/`zip`/`flatten`/`dedup`/`unzip`/`windowed`/`pairwise`/`chunk_by_size` now lower on the compiled tiers (i64-element), and `iter::reversed(xs)[i]` / `for x in iter::reversed(xs)` no longer segfault (the vec-returning intrinsics pin a heap `Vec<elem>` dest, and the for-loop element type is propagated from an inline iterable expression).
- **`sync` extras.** `sync::Once::call` (runs its closure exactly once, the VM builtin now actually invokes the closure), `sync::Barrier::new`/`wait`, and the qualified free-call atomic forms (`sync::AtomicI64::fetch_add(a, n)` etc.).
- **Misc compiled-tier gaps.** `thread::num_cpus`, `encoding::json::valid`, `bufio::read_lines`, `math::min`/`max`/`clamp` in the `math::`-qualified spelling, the `math::PI`/`E`/… constants in arithmetic and print context, `BTreeMap::keys()`, and `strings::pad_left`/`pad_right` (the pad glyph is a String char with a default space).
- **Fundamental String building.** `String::new()` / `String::with_capacity`, `s.push(char)`, `s.push_str(str)`, and `s.chars()` (`for ch in s.chars()`) are implemented across all tiers - they were documented but unwired, and `s.push` previously clobbered its receiver via the mutating-method writeback.
- **`http::Request` request-scoped values + typed/raw path params + `middleware::bearer_ok`.** `r.set_value(k, v)` / `r.value(k)` carry per-request data (a Go `context.WithValue` analog), and `middleware::bearer_ok(r, verify)` runs a Gossamer verify closure across the FFI to gate a route.
- **`http::session` / `csrf` / `cookie` core**, **`http::static_files::FileServer` on the interpreter**, and **`http::websocket::accept`** (the RFC 6455 server handshake) now have compiled-tier coverage and fixtures.
- **`html::escape` is CSP-grade.** It now escapes the OWASP "HTML element content" defensive set - the five metacharacters (`& < > " '`) plus `/` (`&#x2F;`, the closing-tag / unquoted-attribute edge cases) and backtick (`&#x60;`, IE's attribute delimiter) - so a single `html::escape(value)` is safe in HTML text *and* quoted/unquoted attribute values without the caller knowing the sink. Both tiers escape identically and `html::unescape` round-trips the new hex references. (Context-aware escaping for URL/JS/CSS sinks remains the `html::template` engine's job - a single escaper cannot be context-aware.)
- **`http::Client` cookie jar + proxy (Go's `net/http/cookiejar` / `Transport.Proxy`).** `Client::builder().cookie_jar(true)` builds a client that holds a persistent engine - one `ureq::Agent` on the boxed client (compiled tiers) / an id-keyed `gossamer_std::http::Client` registry (interpreter) - so a `Set-Cookie` stored on one request is re-sent on the next request made with the same client. `cookie_jar(false)` (the default) runs each request on a fresh engine, so nothing carries over. `.proxy(url)` routes every request through an HTTP/SOCKS proxy. Both builder methods chain across every tier; the verb-chain (`client.get(url)....send()`) and `client.request` paths all run on the client's engine. Fixture `feature-testing-examples/http_client_cookie_jar.gos` proves the jar round-trips bit-identically on the VM, Cranelift, and LLVM.

### Correctness - compiled-tier silent-divergence and codegen fixes

- **`value_to_bytes` accepts a byte-array literal.** The interpreter's crypto helpers read `[u8]` arguments via `value_to_bytes`, which only handled the boxed `Array` shape and returned empty for the packed `IntArray` a `[112, 97, …]` literal lowers to - so VM crypto (`pbkdf2`/`argon2id`/`aead`) silently hashed *nothing*. The `IntArray` arm fixes the whole family.
- **`.len()` on a runtime-helper String temporary.** `crypto::sha256::hex(x).len()` (and the other hex/`unicode::nfc` temporaries) read a `GosVec` header out of a c-string pointer on the compiled tier because the inline-receiver dispatch keyed on the unresolved HIR type; it now re-keys off the resolved (`String`) type and routes to `gos_rt_str_len`.
- **`iter::min`/`max` carry a real `Option<i64>`.** The shims returned a bare `i64` while the static type was `Option<i64>`, so `iter::min(xs) |> option::default(0)` read garbage through the Option ABI; the shims now return the boxed Option.
- **`HashMap<String, Vec<i64>>` get/or_insert.** A String-keyed, aggregate-valued map stored via the i64-keyed path and could never be read back, and `or_insert` build-failed; dispatch now routes by key kind, with RC blob retain/release parity, and an `or_insert` of a fresh aggregate no longer double-frees at teardown.
- **`fs::metadata(p)` returns a struct.** Field access (`m.size`, `m.is_file`) segfaulted on the compiled tier (the shim returned only the size as `Result<i64>`); it now uses the injected-source-struct pattern (a `__gos_fs_Metadata` wrapper over a 6-tuple leaf), matching the VM's `fs::Metadata`.
- **`flag::Set::parse([literal])`** no longer segfaults - the `[String]` array literal is coerced to a heap `GosVec` before the shim (the same whitelist `http::Client::request` uses).
- **Closures inside `impl`/`trait` method bodies work on the compiled tiers.** The closure-lifting pass never descended into `impl`/`trait` method bodies, so a closure inside a handler's `serve` method lowered to a null environment; the lifter now walks those bodies.
- **`slog::info`/`warn`/`error`/`debug` emit structured JSON on every tier.** The compiled shims printed `INFO: msg` plain text and dropped fields; they now emit the same `{"level":…,"msg":…}` line the interpreter does.
- **More cross-goroutine sync registries made global.** `BARRIER_REGISTRY` and `ATOMIC_U64_REGISTRY` were still `thread_local!` (so they silently lost updates across goroutine worker threads); both are now process-global like the rest.

### Systemic - stdlib tests now compile-and-run

- **`assert_vm_output` retired.** The `stdlib_new_modules` probe helper ran `gos` only - the reason VM-only drift went uncaught across the standard library. It now also `gos build`s each probe, runs the native binary, and asserts the compiled stdout matches the VM bit-for-bit (an explicit `assert_vm_only(reason)` exists for documented exceptions). Folding the ~52 probes into the cross-tier gate surfaced and fixed several more compiled-tier gaps at once.
- **`static_files` conditional GET** parses the RFC 1123 / RFC 850 / asctime date formats browsers actually send for `If-Modified-Since` (was RFC 3339 only), returning 304 correctly.
- **Canonical web example.** `examples/web_auth_api.gos` shows a router with path params, a `middleware::bearer_ok` auth gate, and `session::sign`/`verify`, running identically on every tier.
- **CI parity battery split into groups.** The single "every example" parity walks (`*_matches_vm_on_every_example`, the strict-lowering check) are now `cranelift_parity_group_N` / `llvm_parity_group_N` / `llvm_strict_lower_group_N` (round-robin across `PARITY_GROUPS`), so a failing example fails a small, fast group test that still names the example, instead of one giant suite.
- **Per-platform workspace tests run as labelled groups.** The matrix `test` job's single `cargo test --workspace` step is split into named area groups (frontend, IR & codegen, runtime & interpreter, stdlib & tooling), each `if: !cancelled()` so a failure in one still runs the rest and the job reports every area. On each platform a failure now lands in its own collapsible, labelled log section instead of being buried in one giant block after a platform-specific error; the final group is `--workspace --exclude` of everything already run so a newly added crate is always covered.

### Weekly Miri + fuzz CI

- **Miri runs the `gossamer-runtime` crate clean.** The `mimalloc` global allocator and the RC pool's direct `mi_*` calls fall back to the system allocator under Miri (as they already do under ThreadSanitizer), so Miri models every allocation instead of aborting on the first foreign call. The strict-provenance defects it then surfaced are fixed at the root: the closure-env and flat-slot ABIs recover a pointer's exposed provenance (`with_exposed_provenance`) rather than `transmute`-ing an integer, and the `Vec` element buffer is word-aligned (its `i64` / pointer slots were being read through a 1-aligned `Vec<u8>`). Socket round-trip tests are skipped under Miri (no sockets there), and the million-node release stress shrinks under Miri while the native run is unchanged.
- **The weekly fuzz job survives the full hour.** The process-global symbol interner is now resettable (`reset_interner`) and the fuzz harnesses clear it per input, so a long run no longer accumulates every identifier ever interned - the same unbounded growth the long-lived LSP shared. The fuzz binary falls back to the system allocator (a `--cfg fuzzing` gate) so ASan / LeakSanitizer instrument the heap, and the weekly targets run in libFuzzer fork mode (`-fork=1`) so a child that trips the RSS cap is recorded and replaced instead of killing the parent.
- **Bytecode-VM user-function inliner.** `gos` re-compiles a small non-recursive single-expression helper directly at its call sites instead of emitting a per-call frame, keeping the result in its native (`i64` / `f64` / value) register bank. A function that calls `panic` / `assert` stays a real frame so panic and failed-test tracebacks are unchanged, and `gos test` disables inlining to preserve the full call chain. Spectral-norm runs ~4.8× faster interpreted; `GOSSAMER_INLINE=0` turns it off for differential checks.
- **Tuple-extracted RC values are released, and destructuring loops auto-region.** A value bound out of a tuple (`let (tree, _) = build()`) is now retained at the extract and released with its owner, and a loop body that only produces and consumes fresh per-iteration values is bump-allocated and freed wholesale. The ast-rewrite cross-round leak is gone (≈487 MB → ≈49 MB at depth 20) with no speed cost.
- **`&mut String` write-back works on every tier.** `*s = v` (release-old) and `*s += v` (self-consuming append) through a `&mut String` parameter now persist - the VM via its cell protocol, the compiled tiers via a by-slot-address `Rvalue::Ref` plus a post-call reload - enabling the idiomatic `serialize_into(&mut buf)` accumulator that the JSON benchmark now uses.
- **Counted-loop `[i64]` reads skip the bounds check on the LLVM tier.** `for x in vec` over a primitive-element vector lowers to a branch-free unchecked element read, since the induction index is provably within `[0, len)`.
- **Cleanup.** Removed the dead heap-`GosResult` exit-code fallback (Result/Option are by-value `i128`), fixed broken rustdoc intra-doc links so `cargo doc -D rustdoc::broken_intra_doc_links` passes, and the `crypto::hmac::sha256_mac` lowering test now passes byte slices (`.as_bytes()`) like the other byte-input crypto helpers instead of a `String` (which the compiled shim read as a byte vector).

## 0.13.0 - Bidirectional typechecking, identity-operand folding; std::database::sql end-to-end; rust-bindings native builds, deep fixes, tier parity and correctness.

- **The typechecker is formally bidirectional.** Expected types flow *down* into expressions as they are checked (an `Expectation` threaded through the checker, rustc-style) instead of being retro-patched onto literal nodes after unification. Every site that knows its expected type propagates it: annotated `let` / `const` / `static` initializers, function and closure return positions (block tails, `return`, branches, match arms), call and method arguments, variant-constructor payloads, struct-literal fields, and `&`-borrows. Literal containers adopt the expected shape on first pass - fixed `[T; N]` versus heap `Vec<T>` is a layout decision unification cannot rewrite later - and element mismatches now surface at the element's own span.
- **Assignment values are checked against the place's type.** `v = [2, 3]` into a `Vec<i64>` slot previously recorded the literal as a fixed `[i64; 2]` - a layout desync from the Vec-typed slot on the compiled tiers. The place type now flows into the value as its expectation.
- **`Some` / `Ok` / `Err` payload literals adopt the expected payload shape.** `let x: Option<Vec<i64>> = Some([1, 2])` previously recorded the payload as a fixed `[i64; 2]` inside a `Vec`-typed slot; the constructor now threads the expected `Option` / `Result` payload type into its argument.
- **Identity-operand constant folding in MIR.** A `BinaryOp` with one constant identity or absorbing operand folds away: `x + 0`, `x - 0`, `x * 1`, `x / 1`, `x | 0`, `x ^ 0`, `x << 0`, `x >> 0` fold to `x`; `x * 0`, `x & 0`, `x % 1` fold to `0`; `b & true`, `b | false`, `b ^ false` fold to `b`; `b & false` / `b | true` fold to the constant. Integer and bool only - float identities are unsound under IEEE-754, and a non-constant divisor keeps its runtime division (`0 / x` still faults when `x == 0`). Pays off where LLVM `-O3` never runs: the bytecode VM and unoptimised `gos build` binaries.

`std::database::sql` is now callable from Gossamer source on every tier. The 0.9.0 release shipped the driver registry, the Rust trait surface, and 33 `gos_rt_sql_*` shims - but no front half: `sql::open(...)` failed under `gos` (no interpreter binding) and `gos build` emitted undefined `@sql::open` symbols. The full CRUD path (parameterized execute/query, row iteration, typed getters, transactions, savepoints, isolation levels) now produces bit-identical output under `gos`, `gos build`, and `gos build --release` against a live PostgreSQL via the external pgooseql driver.

- **Injected real-struct wrappers for `database::sql`** (the pem/x509/tar/zip precedent): `gossamer-parse` injects Gossamer source defining `Conn` / `Rows` / `Row` / `Tx` as one-field handle structs, the `Value` / `IsolationLevel` enums, and `sql::open` / `sql::drivers` wrappers whose bodies call scalar-shaped `__gos_sql_*_raw` leaf intrinsics. Methods on the injected structs are ordinary impl methods, so every tier executes the same code; the dead `stdlib_sql.rs` MIR dispatch tables (never wired into method lowering) are deleted. `sql::Value::Int(1)` / `sql::IsolationLevel::Serializable` variant paths and the `sql::Error` → `errors::Error` type alias rewrite at parse time.
- **One semantics for all tiers**: the interpreter's `__gos_sql_*_raw` builtins and the C-ABI shims marshal to the same safe core functions in `c_abi/sql.rs` - one handle registry, identical sentinel conventions. New shims: parameter binding (`gos_rt_sql_params_new` / `_push_{null,bool,int,float,text,blob}`, `gos_rt_sql_conn_{execute,query}_params`), `gos_rt_sql_conn_close`, `gos_rt_sql_row_kind` (typed-getter mismatch checks), `gos_rt_sql_row_get_bool_i64` / `_get_blob_vec`, and `gos_rt_sql_last_error` - every failure path now records a message (previously `Err(_) => -1` discarded it).
- **The SQL driver registry is shared across linked runtime copies.** A `gos build` binary with `[rust-bindings]` links two gossamer-runtime copies (`--allow-multiple-definition`); the registry static was duplicated per copy, so a driver registered by the binding was invisible to `gos_rt_sql_open`. The storage now lives behind one unmangled symbol (`GOS_RT_SQL_DRIVER_REGISTRY`) the linker deduplicates.
- **`gos build` works for `[rust-bindings]` programs.** Calls to external binding fns emitted no LLVM `declare`, so every `[rust-bindings]` program failed at the `opt` stage with `use of undefined value '@gos_binding_…'`. The emitter now synthesizes the declaration from the call-site types (which the MIR binding lowering derives from the binding's signature metadata).
- **`gos build --release` works for `[rust-bindings]` programs.** The static-musl release link consumed a glibc-built bindings staticlib (undefined `__res_init` / `open64` / `gnu_get_libc_version`); the bindings staticlib is now built with `--target x86_64-unknown-linux-musl` when the main link is static-musl, with the cargo target folded into the archive path and freshness stamp.
- **Array/tuple literal arguments re-type against method and variant-constructor parameters.** The 0.10.0 re-typing covered free-fn calls only: `c.execute(&[V::I(1)])` (method) and `Value::Blob([1, 2, 3])` (variant ctor) kept the fixed `[T; N]` layout while the callee read a heap GosVec - a native-tier segfault. Method signatures are recorded by name + arity (conflicting registrations are poisoned, never coerced); `&`-borrowed literals re-type through the reference; explicit `return [..]` re-types against the declared return like the block-tail path already did.
- **Nested by-value enums (`Result<Option<T>, E>`) round-trip on the compiled tiers.** `Ok(Some(2))` truncated the inner 2-word Option to its discriminant word (`while let Some(v) = f()?` yielded zeros for scalars and segfaulted for struct payloads). Construction now heap-copies a 2-word by-value enum payload (`maybe_heap_copy_value_enum`); the new `gos_rt_result_payload_i128` extractor dereferences it back by value; `None` in return position pins its destination to the i128 representation (`lower_result_no_payload` mirrored `lower_result_ctor`).
- **`.len()` on a fixed `[T; N]` array is a compile-time constant.** It previously routed to `gos_rt_len`, which read a GosVec header out of the inline stack aggregate - `[1, 2, 3].len()` returned 1 natively (3 on the VM).
- **Fixture**: `feature-testing-examples/sql_driverless.gos` exercises the registry, the unknown-driver error path through `?`, and `Value` / `IsolationLevel` construction + matching without a database, registered for tier parity.
- **Struct-receiver `.close()` no longer hijacked by the channel helper.** The bare-name method table routed every `.close()` to `gos_rt_chan_close` regardless of receiver; `rows.close()` on the compiled tiers closed a bogus channel handle (and could deadlock at startup in larger binaries). The arm now carries the same receiver gate as `insert` / `get`: a struct whose impl defines `close` dispatches to it, everything else keeps the channel lowering.
- **SQL cursors no longer leak handles.** `Rows` follows cursor semantics: advancing frees the previous `Row` handle (typed getters on a stale row report "row is no longer valid"), a fully drained iteration reclaims everything, and the new idempotent `rows.close()` (wired VM / Cranelift / LLVM) handles early exits - `defer rows.close()` is the idiom, matching Go. `conn.close()` sweeps any cursors still open on the connection, so an abandoned iteration is bounded by connection lifetime. Previously every fetched row leaked its registry entry until process exit.
- **`Conn::query_each(sql, params, f)`** runs `f` once per row with the cursor opened, drained, and closed inside the call - implemented in the injected Gossamer wrapper (defer + let-else drain), so all tiers share one body. Stub-driver unit tests pin the lifecycle invariants; `sql_driverless.gos` exercises `close` idempotency and the `query_each` error path across tiers.
- **`result::default` / `option::default` work on the compiled tiers.** `result::default` had no MIR lowering at all (`gos build` failed at `opt` with `use of undefined value '@result::default'`), and both combinators erased heap / float payload types to raw i64 (a String came back as its pointer digits, an f64 fallback as 0). New `gos_rt_result_default` / `gos_rt_result_default_f64` / `gos_rt_option_default_f64` shims; the destination type is recovered from the scrutinee's `substs[0]` when the call expression's type is still an inference variable. Fixtures `result_default.gos` / `option_default.gos` registered for tier parity.
- **Binding `Result` / `Option` returns are real values on the compiled tiers.** A `[rust-bindings]` fn returning `Result<String, String>` handed the native program a raw binding-ABI `GosVariant` pointer - `match` read garbage payloads and printed pointer digits (the VM was fine). The MIR binding lowering now routes such returns through the new `gos_rt_binding_variant_to_result` shim, which repacks the variant as the runtime's i128 result and re-allocates string payloads as header'd runtime strings, typed as the genuine `Result` / `Option` Adt.
- **Bare `gos` / `gos build` work in any project with an entry source.** Entry resolution from a project root now tries `src/main.gos`, `main.gos`, the manifest-id-named source (`src/<id-tail>.gos`, `<id-tail>.gos`), then a sole `.gos` candidate under `src/` or the root (scratch `_*.gos` and `*_test.gos` excluded). Several nameless candidates produce an error that lists them; previously anything but `main.gos` failed with "pass a path explicitly".
- **Imported binding fns dispatch to the module the program imported.** Eight tuigoose modules each expose `with_block`; the bare-leaf dispatch tables (interp and MIR) disambiguate by arity only, so `use tuigoose::paragraph::with_block` + a bare call could silently route to whichever same-arity candidate the linker registered first - link-order-dependent wrong behavior. HIR lowering now expands a single-segment imported name to its full qualified path when it targets a registered `[rust-bindings]` item (std / user imports untouched), so both tiers dispatch through the qualified entry.
- **New example: `examples/projects/clipboard_rust`** - wraps the published `arboard` crate behind a two-fn `[rust-bindings]` crate (`clipboard::get_text` / `set_text`), prints the previous clipboard text and replaces it with the CLI args. The Linux holder-process dance (X11 selections die with their owner) is handled inside the binding with a claim-acknowledged handshake, so back-to-back invocations are deterministic. When the native path is unreachable (Wayland-only, Termux, headless) the binding falls back to wl-copy / wl-paste, xclip, xsel, then termux-clipboard-get/set; only when none exist does it fail, with a message asking for a clipboard utility to be installed.
- **The retired tracing GC is deleted end-to-end.** The `gossamer-gc` crate, the runtime's handle heap (write barriers, safepoints, shadow stacks, concurrent mark), the MIR `insert_gc_barriers` pass and `GcWriteBarrier` statement, and every dead registry / symbol-table / JIT-dispatch entry are gone. Memory management is what actually ships: reference counting + the drop pass + arenas. SPEC §7.2 and the runtime design doc now describe that model; the live aggregate-malloc shims (`gos_rt_gc_alloc` / `gos_rt_aggr_*`) and no-op ABI-compat entries are unchanged.
- **`std::runtime::mem_stats` removed** - declared in the manifest but implemented on no tier, and its backing stats read the always-empty tracing heap.
- **`regex::compile` returns a real `Result` on the compiled tiers.** An invalid pattern previously came back as `Ok` with a null handle under `gos build` (the VM correctly returned `Err`); the new `gos_rt_regex_compile_result` shim carries the same diagnostic string as the VM, so all tiers report identical errors.
- **`fs::walk_dir` is tier-uniform.** The VM materialised flat path strings while the compiled tiers produced `DirInfo` aggregates; the VM now yields the same `DirInfo` shape as `fs::list_dir`. An unreadable directory (including a missing root) is an `Err` on every tier (the compiled walk silently returned `Ok([])`), and the Err payload is typed as `errors::Error` so `println!("{e}")` renders the message instead of a raw pointer.
- **Block comments no longer nest**, matching SPEC §Comments - the first `*/` closes the comment, so `/* see target/* */` no longer swallows the rest of the file. Tokenization errors (unterminated comment / string, bad escape, …) surface as parse diagnostics (`GP0018`); previously they were dropped with the lexer and a file with an unterminated `/*` parsed as empty-but-valid.
- **`[rust-bindings]` functions can be passed as values.** A value-position reference (`run(config, set_text)`) eta-expands during closure lifting into the equivalent closed closure, so it flows through the standard binding-call lowering - argument and return conversions included - on every tier; previously the compiled tiers emitted a reference to an undefined symbol and died inside `opt`. A binding `FnRef` that somehow survives to the LLVM tier now fails strict lowering with an actionable message instead of an `opt` crash.
- **Explicit `return` values unify against the declared return type.** `return s` with `s: String` in a `-> i64` fn now reports `GT0001` like the equivalent block tail; the return expectation previously shaped literal containers only, so non-literal returns slipped the checker.
- **Lint fixes**: `unused_mut_variable` counts mutating method calls (`xs.push(v)`, `flags.parse(args)`) and `&mut` borrows as mutations; every lint now descends into inline `mod` bodies (`mod tests` was invisible, and `use` imports referenced only there false-positived as unused); `redundant_closure` only suggests passing the function directly when the callee is a locally defined function (imported and qualified-path callees have call-site-only lowerings, so the closure is load-bearing).
- **`gos bench` drops `allocs/op`** - the counter behind it was the removed tracing heap's, so the column always printed a fabricated 0; output is `ns/op` only.
- **The i64 integer model is now uniform across tiers.** The compiled tiers (Cranelift and LLVM) previously routed division, remainder, comparison, and `>>` on unsigned-declared types through unsigned machine ops and printed `u64`/`usize` unsigned, so `(0u8 - 1) / 2`, `0u64 - 1 > 0`, and `z >> 1` diverged from the VM; LLVM also emitted unmasked shifts (`1 << 70` was poison under `-O3`). All ≤64-bit ints now run signed-i64 semantics with `& 63` shift masking on every tier, `u64 as f64` converts signed, and the unsigned printer fires only for explicit `as u64` / `as usize` cast results (matching the VM's `Value::Uint` display provenance). SPEC §3.1 now documents the model.
- **Packed byte-vec helpers honor the element stride.** `first` / `last` / `index_of` / `count_of` / `contains` / `Vec::remove` and indexed get/set on an `elem_bytes=1` vec (e.g. `os::read_file` bytes) read 8 bytes per element on the compiled tiers - out-of-bounds reads and garbage values; `reversed()` also lost the byte element type. All of them now load/store at the header's stride, and `reversed()` preserves the receiver's element type.
- **`xs.pop()` returns a real `Option<T>` on every tier.** The VM evaluated `xs.pop()` to the shortened array itself (so `if let Some(v) = xs.pop()` never matched), and the compiled tiers returned a bare flag with the length untouched. Pop now shortens the receiver and yields `Some(last)` / `None` everywhere (new `gos_rt_vec_pop_opt` shim, stride-aware).

### `std::database::sql` - full surface from Gossamer on every tier

The remaining Rust-façade-only pieces now work from Gossamer source, verified bit-identically under `gos`, `gos build`, and `gos build --release` against live PostgreSQL via the external pgooseql driver (TLS, streaming rows, COPY, LISTEN/NOTIFY, rich type decoding on the driver side).

- **Capability-gated trait extensions** (`gossamer-runtime::sql`): `ConnectionImpl::{copy_in, copy_out, listen, unlisten, poll_notification}` and `TransactionImpl::{execute_params, query_params}`, each defaulting to an honest `driver("sql", "… not supported by this driver")` error; a `Notification { channel, payload, process_id }` type; façade `Conn` / `Tx` wrappers to match.
- **Prepared statements**: `conn.prepare(sql) -> Stmt` with `execute` / `query` / `close`; Stmt handles register under their connection and sweep with `conn.close()`.
- **Parameterized transactions**: `tx.execute_params(sql, &[Value…])` and `tx.query(sql, &[Value…])` (cursors register under the transaction's connection).
- **COPY**: `conn.copy_in(sql, data: &[u8]) -> i64` and `conn.copy_out(sql) -> [u8]` (two-step run/take shims so the wrapper branches on a scalar status before materializing bytes).
- **LISTEN/NOTIFY**: `conn.listen/unlisten(channel)` and `conn.poll_notification(timeout_ms) -> Option<Notification>` over a per-connection last-notification slot read by scalar getter shims.
- **Connection pool relocated to `gossamer-runtime::sql_pool`** (operating on `Box<dyn ConnectionImpl>`; the std façade keeps its `Conn` wrapper + statement-cache LRU as an adapter) and exposed to Gossamer: `Pool::open(driver, url, max)` / `Pool::open_with(…)`, `pool.acquire() -> Conn` (named `acquire`, not `get` - the VM's bare-name `get` router hijacks `get` on struct receivers), `live` / `idle` / `close_idle`; closing a pooled checkout returns it to the pool through the ordinary `conn.close()` shim. Evictions and `close_idle` drop driver connections outside the pool lock.
- **Migrations relocated to `gossamer-runtime::sql_migrate`** (std façade delegates) and exposed as `sql::migrate::up(&mut conn, dir) -> i64`. The `schema_migrations` bookkeeping column is now `BIGINT` - PostgreSQL's `INTEGER` is 32-bit and epoch milliseconds overflowed it (SQLite reads both as i64).
- **`Select` builder in pure injected Gossamer** - fluent `sql::Select::new(t).columns(&[…]).where_eq(col, v).order_by(col, asc).limit(n).offset(n)` rendering `$N` placeholders via `render()` + `params()`; methods return a fresh builder (functional style - correct under both copy and share field semantics).
- **Method-call typing fixes surfaced by the above** (general, not SQL-specific): array/tuple-literal arguments re-type against method and variant-constructor parameters with multi-candidate disambiguation (same name + arity coerce only when exactly one candidate is container-shaped); non-generic impl methods record their declared return type keyed by `(self type, name, arity)` so chained results (`sel.params()`) reach codegen typed; the LLVM call emitter passes one-slot enum arguments by slot address when inference left the call-site local untyped (the callee memcpys from the address - passing the loaded tagged-pointer value made it dereference tag bits).
- **RC accounting no longer depends on item order** (soundness, compiled tiers). `register_rc_managed_ty` only fired at enum-constructor lowering sites, so any body lowered *before* an enum's first constructor treated that enum's locals as non-RC and skipped every retain/release - a by-value enum argument stored into a container inside such a body (the SELECT builder's `where_eq` pushing a `Value`) was freed by the caller's ctor-temp release while the container still referenced it. Symptom: intermittent (allocator-reuse-dependent) garbage discriminants / segfaults in `gos build [--release]` binaries, ~50% crash rate on the pgooseql full example. Payload-bearing enum defs are now registered eagerly during typechecking (`register_rc_managed_enum_def`, def-based so every instantiation of a generic enum is covered); all-unit enums stay excluded (they lower as bare `i64` discriminants). Regression-gated by `feature-testing-examples/enum_param_rc_repro.gos` in tier parity.
- **23 new `gos_rt_sql_*` shims** with ABI-registry entries, JIT symbol-table entries, interpreter builtins over the same safe core, and MIR dispatch arms; driverless tier-parity fixture extended to the Select builder and pool error paths.

### `std::http` - proxy-grade client and server on every tier

The HTTP stack was the largest remaining VM-only / tier-divergent surface; it is now bit-identical under `gos`, `gos build`, and `gos build --release`, pinned by new live-loopback fixtures in tier parity (`http_redirect_policy`, `http_response_headers`, `http_request_headers`, `http_raw_bytes`, `http_next_chunk`, `http_proxy_stream`, `http_bare_handler`, `http_serve_err_binding`, `http_surface`, `http_roundtrip`).

- **`http::request` / `http::request_bytes` are native on the compiled tiers** (previously VM-only), and every client verb now routes through one ureq engine on all tiers - the hand-rolled native GET paths are deleted - so transport-error strings are identical between `gos` and `gos build`.
- **Client `Response` carries a real `headers` field**: `[(String, String)]` with lowercase names, wire order, duplicates preserved. The `content_type` / `location` accessors are fixed on the compiled tiers (they read heap garbage before).
- **Configured clients: `http::Client::builder().max_redirects(n).timeout_ms(ms).build()`** plus `client.request` / `client.request_bytes`. `max_redirects(0)` hands back the raw 3xx with its `Location` header intact - the proxy-correct mode.
- **The legacy builder chain (`.get(url)` / `.post(url)` / `.header(k, v)` / `.body(s)` / `.send()`) is tier-unified**: `send()` returns `Result` everywhere, `.header` / `.body` are honored on the VM (they were silently dropped), and `put` / `options` / `delete` / `head` have native shims (previously VM-only).
- **Server `Request.raw_body`** exposes the inbound body as bytes (`[u8]`). Inbound headers are populated and `r.headers` works natively as a real `Vec<(String, String)>` - previously empty on the VM path and a segfault on the compiled tiers (no native lowering existed for the field projection). `request.path` strips the query string natively (`r.query` keeps it).
- **Handler-set response headers reach the wire**, and `with_header(k, v)` chains with replace-then-push semantics (a repeated case-insensitive name keeps the last value). Content-type precedence is explicit header > constructor field > `text/plain` default on every tier - the compiled `Response::text()` emitted `application/json` before. A struct-literal `http::Response` lowers natively, and handlers declaring a bare `http::Response` return (no `Result`) are wrapped natively via a synthesized `__ok_wrap` thunk - the native server previously misread the Response pointer as a Result discriminant and answered 500.
- **Streamed server responses**: `Response::stream(status, content_type, upstream)` writes status + headers + `Transfer-Encoding: chunked` and drains the upstream `ResponseStream` to the client in 8 KiB frames with no full-body buffering - the proxy-passthrough shape. On the client, `ResponseStream::next_chunk(max) -> Option<[u8]>` reads raw byte frames (packed `elem_bytes=1` vec), complementing the line-oriented `next_line()`.
- **Compiled h1 server hardening parity**: the native server now reads request bodies via `Content-Length` (it never read POST bodies before), decodes inbound chunked transfer encoding + trailers, enforces the 1 MiB default body cap on every tier (the native cap was a divergent 64 MiB), answers 413 to a hostile `Content-Length`, keeps NUL bytes in byte bodies intact, and emits lowercase wire header names matching the VM.
- **`http::serve` returns `Result<(), Error>` on every tier** - a bind failure is the caller's `Err` value. Previously the interpreter panicked and a native binary aborted; matching on the serve result also broke LLVM lowering ("sext void to i64") when the call lowered to a unit destination.
- **Error rendering parity**: `println!("{e}")` on a wrapped error renders the colon-joined cause chain (`outer: mid: root`) on every tier; the `errors::join` separator is unified to `"; "`; a let-bound `errors::new` value printed as pointer digits natively before.

### Tier-parity correctness sweep

- **Scheduler: wakes are no longer lost across worker retirement.** `unpark` delivered a resurrected goroutine into its home worker's inbox without checking whether that worker had retired (pool shrink via `runtime::set_max_procs`), stranding the task forever; retiring workers also abandoned queued inbox/deque tasks. Both paths now hand off under the documented retired-inbox lock-ordering invariant, with deterministic regression tests covering the exact interleaving. Symptom: a parked `runtime_future::drive` goroutine missing its `Waker::wake` under concurrent pool resizes.
- **`regex::compile(..)` results keep their pattern identity through `unwrap`.** The Result-shaped `gos_rt_regex_compile_result` rewire missed the runtime-kind rows, so `pat.replace_all(..)` on the compiled tiers lowered to an undefined bare symbol (and `pat.replace(..)` silently misrouted to the string helper). The kind map covers the new symbol and the payload-extracting result helpers (`unwrap`, `unwrap_or`) propagate the receiver's kind.
- **Closure parameters are typed from std combinator signatures.** An unannotated `|e| …` passed to `result::map_err`, `option::map`, and the rest of the tabled `result::` / `option::` / `iter::` surface pins to the payload type; the compiled tiers previously defaulted untyped closure params to `i64` and printed `String` / `Error` payloads as raw pointers. A closure passed to a combinator with no signature row is rejected uniformly (`GT0013`) instead of miscompiling. Fixtures: `combinator_sweep.gos`, `closure_payload_typing.gos`.
- **Std free functions work as first-class values on the compiled tiers** (`r.map_err(errors::new)`): a table maps each supported path to a word-shaped `gos_rt_*` symbol the existing eta-expansion machinery points its thunk at; untabled names are rejected uniformly (`GT0015`) with a wrap-in-a-closure suggestion. 30+ `result::` / `option::` / `iter::` combinators gained native dispatch in the same sweep (previously VM-only).
- **`&mut Vec<T>` / `&mut [T]` parameters write through on the VM** via writeback cells - element writes, growth via `push` (realloc visible to the caller), `swap`, nested-call forwarding, early-return paths, struct-field places, and closures taking the `&mut` param all mutate the caller's storage. Mutations through such params were silently lost before. Fixture: `mut_ref_params.gos`.
- **The full `as` cast whitelist runs on every tier**: `bool` / `char` sources, `f32`, float → int truncation toward zero with saturation at i64 width and NaN → 0 (no narrow mask: `300.7 as u8 == 300`), integer narrow-width masking (`300 as u8 == 44`), and unsigned display provenance - the VM previously no-op'd several whitelisted shapes. `i128` / `u128` are rejected uniformly (`GT0014`): no tier has a 128-bit runtime representation, so the binding previously truncated silently.
- **GosVec slot-children deep-free.** Breaking early out of a loop over regex capture / response-header / csv / json materializer results leaked the elements' c-string children (the guarded-meta walk stopped at the slot); the release walk now frees slot children at every depth, and a companion double-free in the drop pass is fixed. Fixture: `early_break_materializers.gos`.
- **`gos fmt` is faithful.** The AST pretty-printer - which destroyed sources (comments and macro bodies dropped) - is replaced by a token-stream formatter: comments, macro calls, and authored line structure are preserved verbatim while spacing and indentation normalise; output is idempotent; and a no-destruction self-check re-lexes the result and refuses to write anything whose non-whitespace token stream differs from the input's.
- **Parser / driver fixes**: struct literals parse in delimited scrutinee positions; `GL0010` no longer false-positives on an else-less `if let`; a bare manifest id in source is a hard error and calling through an unbound binding name raises `GX0002` (both silently produced Unit before); the binding ABI's GosVec readers honor packed element strides; the runner template pins its `time` dependency (the cookie × time `E0119` build break); `super::` paths resolve inside `#[cfg(test)]` modules; `fs` error text matches across tiers (fixture `fs_error_text.gos`).
- **Win64 i128/aggregate ABI follows the target triple.** The LLVM backend gated its Win64 fat-aggregate (`<16 x i8>`) marshalling on the host `cfg!(windows)` rather than the resolved target triple, so cross-building to a Windows target from a non-Windows host emitted SysV ABI against a Win64 runtime; it now keys off `host_triple()`, matching the llc target and datalayout.
- **`println!` flushes on every newline.** `gos_rt_println` now unconditionally flushes stdout after the newline (matching Rust's `LineWriter` contract), so native-program `println!` output appears immediately without a manual `io::stdout().flush()`. `print!` (no newline) keeps its unbuffered-until-newline behavior, as in Rust.
- **SKILL.md trimmed** from ~50k to ~34k chars - stdlib surface compressed and style rules de-duplicated against the idioms section, with every syntax rule and API entry preserved.

### Security

- **Package tarball extraction rejects path traversal.** `tar::unpack` refuses entries with absolute or `..` paths (and Windows separators), so a malicious package can no longer write outside the cache through `gos add` / `fetch` / `vendor`.
- **Registry tarballs are Ed25519-verified before extraction.** A registry source must carry a valid publisher signature over the tarball; the publisher key is pinned in `project.lock` on first fetch and a later key change is rejected, and an unsigned registry source is refused.
- **Plaintext registry traffic no longer downgrades silently.** An `http://` registry or download URL is refused unless the host is loopback or `GOS_ALLOW_INSECURE_REGISTRY=1` is set.
- **HTTP response-header injection is blocked on every tier.** The interpreter and native servers drop any handler-set header whose name or value contains CR / LF / NUL, closing response splitting through a reflected header or cookie value; the cookie encoder also drops CR / LF / `;` / `"` / `\`.
- **The SQL `Select` builder quote-escapes identifiers on every tier.** A table / column / `ORDER BY` identifier that is not a plain identifier is emitted as a single quoted identifier rather than concatenated raw, so a user-controlled sort column cannot inject SQL; savepoint names are validated likewise.
- **`json::encode` escapes `<`, `>`, `&`, U+2028, and U+2029** so a JSON string is safe to embed in an HTML `<script>` block; the escapes round-trip and are byte-identical across tiers.
- **HTML auto-escape template hardened**: a `javascript:` / `data:` URL-attribute scheme is neutralized and unquoted-attribute values escape their terminators, closing two XSS vectors.

## 0.12.0 - Arenas, compact enums, panic ergonomics, predictable memory, deep optimizations.

- **`arena { }` blocks (inspired by Zig).** Everything allocated in the block is bump-allocated and freed wholesale on every exit path (desugars to a block-scoped `defer`). Arenas nest, slabs recycle, and retain/release are no-ops for arena values via a range check against one reserved virtual range. Statement-position only; contract documented in the memory-model chapter. binary-trees with arenas: 0.36 s / 18 MB.
- **Compact heap enums.** The discriminant left the payload: enums with more than 4 variants keep it in a spare header byte; enums with at most 4 carry it in pointer tag bits, so match dispatch reads no memory. The RC header also shrank 16 to 8 bytes (size field deleted - `mi_free` needs only the pointer; meta pointer interned to a 16-bit id). Net: a two-pointer tree node went 48 to 24 bytes (16 inside an arena). Enums are capped at 256 variants (`GT0012`).
- **Loop-carried values release at last use in the iteration.** The old value of a reassigned local was released at the reassignment - after the next structure was already built, doubling transient peaks. A hoisting pass releases at the last mention instead (original site stays as a null-safe backstop). With everything above, binary-trees peak RSS: 165 MB in 0.11.0 to 33 MB - below the Rust and Go ports; ast-rewrite 0.59 s / 163 MB to 0.16 s / 49 MB.
- **Owned heap values release at last use, not function return** (liveness pass with null-out + return backstop; `Weak`-creating functions keep return placement).
- **Escaped struct values reclaim deterministically (the `cycles` leak),** via reference-counted copy blobs with guarded child metas, provenance-set gated so foreign pointers can be leaked but never corrupted. Uniform holder accounting covers overwrite-of-`Some`, `Err`-side payloads, and field aliases.
- **Container-stored aggregates reclaim.** Vec elements with guarded children retain on push/clone/slice and release on free; map blob values release on insert-overwrite/remove/free and the `_opt` getters hand out owned shares (previously they stole the map's).
- **Unit-variant singletons are process-immortal** - fixes the pin being stripped by `let x = Enum::Unit` bindings and a count overflow on arena workloads; also removes two header writes per leaf. Tuple-variant payload detection now comes from the declaration, so matching a unit-variant binding no longer dereferences a bare tag.
- **Strings owned by enum payloads free correctly** - the release walk no longer feeds tag-headered string children into the refcount machinery (was a crash or a leak).
- **Panic ergonomics.** An unobserved `go` panic prints one clean `error[GX0005]` line plus user frames (raw Rust panic line and trampoline frames gone). `runtime::set_panic_hook(f: fn(String))` replaces the default report on every tier. Main-goroutine panics exit 101 (Rust parity, pinned by test) instead of dumping core, and a CI guard rejects `panic = "abort"` in any profile (unwinding is load-bearing for isolation).
- **Process exit no longer boots the scheduler** (~150 ms saved on `Result`-returning `main`); exit waits on the idle condvar, bounded at 5 s.
- **Hot-loop Vec codegen (LLVM):** inline `len`, constant-8 stride scaling, no-grow `push` fast path - 25-40% off pointer-chasing loops.
- **THP RSS inflation fixed:** `allow_thp = 0` set from `.init_array` before the first arena maps.
- **`i128` gets a truthful 8-byte ABI alignment** in emitted modules (16-byte assumption produced faulting vector copies at odd struct offsets).
- **`Vec<bool>` element-stride corruption fixed** (uniform 8-byte slots).
- **RC blocks allocate via `mi_zalloc` directly** - the aligned-entry facade padded every block (~25% RAM tax); and RC metas now emit on the streaming single-unit LLVM path (was an undefined-symbol build error).
- **The bytecode VM JIT-compiles enum-heavy functions.** Heap-enum parameters and returns cross the JIT boundary as native tagged pointers (`Value::NativeEnum` owns the reference; shapes built from the HIR drive VM-side match/field access on handles), so hot recursive-enum code runs at compiled-tier speed under `gos`: binary-trees 44 s to 1.2 s (Go's compiled binary runs 1.1 s), gc-trees to 0.6 s. Mixed-phase calls stay sound - a boxed value falls back to bytecode. Bodies with struct locals or inline-Option i128 locals are declined (bytecode), and one uncompilable body no longer disables the whole JIT module.
- **VM shape tests compare interned pointers.** `VariantIs`/`StructIs` operands moved to an interned-name table; one pointer compare replaces string equality per match arm (13% off enum-heavy VM workloads).
- **VM enum/struct values fused to one allocation + buffer.** `VariantInner`/`StructInner` carry fields inline instead of behind a second `Arc` (VM RSS on tree workloads −19%).
- **`use std::...` paths are validated: a module path that does not exist is an error (`GR0005`).** Imports bind by tail name, so alias spellings (`std::json` for `std::encoding::json`) and outright typos (`std::nonsense`) were silently accepted and only failed at member lookup, or never. Resolution now checks the path against the canonical module table (drift-tested against the std manifest); item imports through a valid parent module (`use std::sync::channel`) are unaffected. The autoderive injection also switched to the canonical `std::encoding::json`.
- **The LSP runs the autoderive step.** Editor diagnostics previously reported every synthesized name (`from_json::<T>` and friends) as unresolved because the LSP parsed raw source while the driver augments it first; the LSP now mirrors the driver pipeline, filters diagnostics pointing into the synthesized tail, and no longer emits duplicate-import noise for the injected `use` lines.
- **`std::option` and `std::result` registered as manifest modules** (they were resolvable but missing from the canonical module table, so `use std::option` wrongly failed the new path validation); examples migrated off the removed `std::exec` / `std::url` alias spellings to `std::os::exec` / `std::net::url`.
- **String concatenation accepts a borrowed right-hand side with a literal left-hand side.** `"hello, " + &name` - the documented spelling - failed to unify (`String != &T`); the checker now peels the reference. The previous `"x".to_string() + &y` form only typechecked by accident through an unresolved inference variable.
- **The tree-walker matches JIT-produced native enum values** (they previously fell through every match arm to unit).
- **Examples and fixtures brought up to current idiom**: compound assignment, no `.to_string()` on literals, no `as usize` on indices, direct `for x in xs` iteration, bare integer literals.
- **Arena slab offsets round to the host page size.** The reserve-range allocator rounded oversized slabs to 4 KiB, so on 16 KiB-page systems (macOS arm64) a following slab started page-misaligned, `mprotect` rejected the commit, and the allocation returned null - a crash the macOS ASan job caught. The page size is queried once (`sysconf` / `GetSystemInfo`).
- **Parsed JSON documents are reclaimed.** `json::parse` / `json::get` handles were leaked boxes (the note said "the GC reclaims them" - the tracing GC is gone), so every parse in a loop leaked the whole document tree. A drop-pass rule frees provably single-owner `json::Value` locals (handle moves through `?` included) via the new `gos_rt_json_free`; aliased or escaping handles keep the old behaviour. `from_json::<T>` loops: 173 MB at 200k iterations to flat.
- **`result_new` destinations with copy-blob payloads are option holders even when the typer left the temp unresolved** - the payload blob no longer leaves a helper one count high, pinned in the collector buffer.
- **By-value `Result<Struct, E>` payloads no longer double-free or dangle.** Three coordinated root fixes: `result_payload` extractions are borrows (the per-field release walk no longer fires for locals that never retained); option-slot releases are not early-relocated for Results whose payload is extracted (the borrow's lifetime is invisible to the liveness pass - Results never extracted-from keep early placement); and `option_slot_release` nulls the payload word so a second slot release is structurally a no-op.
- **Opaque runtime handles excluded from per-field stack accounting.** `http::Response` (and the other sentinel stdlib structs) lower as one-slot handles, but the field walk used their declared field lists - releasing words past the alloca and corrupting the stack. Any `http::get(..)?` in a `Result`-returning helper crashed.
- **Qualified serde spelling restored:** `json::from_json::<T>` (and yaml/toml forms) parse to the synthesized serializers again.
- **Frontend cache keyed by compiler build** - stale ASTs were served across rebuilds with an unchanged version string; the key now folds in a per-build stamp that tracks every frontend crate.

## 0.11.0 - Process isolation, cross platform parity, block scoped defer and derive.

A panic in a spawned goroutine now terminates only that goroutine: the process keeps running and exits cleanly, on every tier (bytecode VM, Cranelift JIT, LLVM AOT). A panic on the main goroutine stays fatal, as in Rust - isolation is goroutine-scoped, not panic-swallowing.

- **Goroutine fault isolation, verified across tiers.** The compiled tier's `gos_rt_panic` contains a panic raised inside a goroutine (the M:N scheduler keeps running other goroutines) and the interpreter catches the runtime error in the goroutine thread. `crates/gossamer-cli/tests/process_isolation.rs` builds and runs both a panic-in-goroutine and a panic-in-main program on `gos` and `gos build`, asserting the process survives the former (and that the goroutine genuinely panicked) and dies on the latter.
- **Buffered stdout is flushed before a fatal panic.** A main-goroutine panic aborts the process; `gos_rt_panic` now flushes the runtime's line-buffered stdout first - as `gos_rt_exit` already does - so output printed before the panic is no longer swallowed by `abort()`.

### Language features

- **`spawn(f)` join handles.** `spawn(f)` runs `f` on a goroutine and returns a `JoinHandle<T>`; `handle.join()` blocks for the outcome as `Result<T, String>` - `Ok(value)` on normal return, or `Err(message)` if the goroutine panicked. Works on every tier (the runtime delivers the panic message to the handle as the stack unwinds, then isolates the goroutine like `go`). Closures may capture their environment. Fixture: `feature-testing-examples/spawn_join.gos`.
- **Real `select { }` on the compiled tiers.** Cranelift and LLVM previously lowered `select` to an "arm 0 always fires" stub; they now poll arms in source order and park the goroutine until one is ready (or a `default` arm fires) via a new `gos_rt_select_*` runtime, matching the VM walker bit-for-bit. Send arms (`tx.send(v) => …`) now parse. Fixture: `feature-testing-examples/select_multiplex.gos`.
- **Block-scoped `defer` (Swift/Zig style).** The reserved-but-no-op `defer` now runs its expression when control leaves the enclosing `{ }` block - fall-through, `return`, `break`, or `continue` - in LIFO order, on every tier. A `defer` in a loop body runs each iteration. Example: `examples/defer_cleanup.gos`.
- **`let PAT = expr else { … }`.** Refutable-let-or-diverge, desugared to a `match` so it runs on every tier. Fixture: `feature-testing-examples/let_else_binding.gos`.
- **`#[derive(Clone, PartialEq, Eq, Default, Debug)]` for structs and enums.** Synthesizes the matching methods as real Gossamer source (the same parse-time path that derives JSON/TOML/YAML), so `==` / `!=` (field-wise), `.clone()`, `Type::default()`, and `{:?}` / `{}` (rendering `Name { field: value }`) work identically on the VM walker, Cranelift, and LLVM. Struct fields may be primitives, `String`, `[T]`, **nested structs**, and the struct may be **generic** (`struct Wrap<T>`). Enums derive too when their variants are all **tuple** (`Circle(f64)`) or **unit** (`Point`) - `Debug` renders `Circle(5.0)` and `Default` picks the `#[default]` variant. Example: `examples/derive.gos`; fixture: `feature-testing-examples/derive_traits.gos`. (Struct-payload enum variants are not yet derivable.)
- **Structs and tuples as `HashMap` / `HashSet` keys.** Keys are now compared and hashed by *value* on every tier: two equal-valued keys at distinct allocations share a slot, a re-insert overwrites, and a distinct key is a distinct slot. Works for flat structs (`struct Point { x, y }`), `String`-field structs, nested structs, and tuples. The compiled tiers hash the key's content via a per-slot layout descriptor (dereferencing `String` fields); the VM keys aggregates structurally - previously it collapsed every aggregate key into a single slot (`len()` of a struct-keyed map was always 1). `#[derive(Hash)]` is accepted on a key type. Fixture: `feature-testing-examples/struct_map_keys.gos`.
- **Collection literals coerce to `Vec<T>` / `[T]`.** `[a, b]` and `[v; N]` build a growable Vec/slice wherever the expected type calls for one - a `let` annotation, a `-> Vec<T>` return, a Vec-typed field, and a Vec/slice argument - on every tier, not only for integer-element literals as before. An `if` / `match` whose branches are literals of differing length joins to `Vec<T>` for every element type. Example: `examples/vec_literals.gos`; fixture: `feature-testing-examples/vec_literal_coercion.gos`.
- **Nested generic types parse.** `Vec<Vec<T>>`, `HashMap<String, Vec<i64>>`, and deeper now close on the maximal-munch `>>` token, which the type / generic-parameter / turbofish parsers split into the per-level `>` (previously a hard parse error).

### Compiled-tier correctness fixes

- **Nested structs by value work on the compiled tiers.** A struct with a struct-typed field (`struct Outer { inner: Inner }`) read garbage for `o.inner.tag` under `gos build` / `--jit` (a 1-slot aggregate field was stored as a pointer and read back inline). Aggregate construction now inlines such fields; multi-slot, deeply-nested, by-argument, by-return, and mutated cases all match the VM.
- **Struct-returning functions no longer corrupt their drop-pass temporaries.** The RC drop inserter typed its throwaway locals from the return slot, so a function returning a struct produced an aggregate-typed `gos_rt_rc_release` destination and a `memcpy` from `null`. It now uses the interned `()` type.
- **Chained field access on a call result resolves its type.** `let a = mk(); a.inner.tag` defaulted the leaf type to a pointer (crash) when `a` came from a struct-returning impl method; copy-type propagation now flows through one field projection, and aggregate-returning callees are no longer inlined (which dropped the type).
- **`Option` / `Result` equality on the VM.** `Some(5) == Some(5)` returned `false` on the VM (variant values weren't compared); enum variants now compare structurally, matching the compiled tiers.
- **Narrow-integer collection indices.** Indexing a growable array with a `u8`/`u16`/`u32`/`i8`/`i16`/`i32` value (`count[b]` where `b: u32`) emitted invalid LLVM IR (`i32` where `i64` was expected) and failed `gos build`; the inline index fast paths now widen the index to `i64` first, matching the VM and Cranelift.
- **Nested `Vec<Vec<T>>` of heap elements no longer double-free.** Indexing a container returns an interior borrow the container still owns; the drop pass scheduled an extra free for it, so a `Vec<Vec<String>>` (literal or built with `push`) crashed at scope end on the compiled tiers. `{:?}` of a `Vec<Vec<String>>` is rendered too.

### Cross-platform parity

- **Windows: user functions returning `Result`/`Option`/inline-enum no longer miscompile.** The Win64 `<16 x i8>` fat-return ABI was applied to user-function calls, not just runtime shims; it is now gated to the ABI registry, with both LLVM call emitters routed through one `needs_win64_fat_ret` decision.
- **`gos build` works from a released install.** Every release artifact (tarball, zip, deb, rpm, Inno Setup, Docker) now ships `libgossamer_runtime.a` / `gossamer_runtime.lib`; the installer places it where `gos build` resolves it, and the cross-compiled Linux-aarch64 / macOS-x86_64 jobs build the runtime for their target.
- **mimalloc is the process allocator** on every platform and binary (toolchain and compiled programs), replacing the platform default - notably musl `malloc` on the static-musl release path. Its page-purge delay is set to zero so freed memory returns to the OS promptly: a phase-structured program (build a large map, drop it, build the next) keeps a flat footprint instead of holding every phase's pages until exit (peak RSS roughly halved on such workloads), at unchanged throughput.
- **Owner-only-DACL modules build on Windows.** The credential and multipart-upload files carry a `#[cfg(windows)]` Win32 ACL block; their module lint moved from `#![forbid(unsafe_code)]` to `#![deny(...)]` so that one audited block compiles under a local `#[allow]` (`forbid` cannot be locally overridden).
- **Windows credential and multipart-upload files get an owner-only DACL**, the analogue of the POSIX `0o600` they already set; the write fails closed rather than leaving a world-readable file.
- **`pid_alive` is accurate on macOS and Windows** (`kill(pid, 0)` / `OpenProcess` + `GetExitCodeProcess`), so a stale build lock from a crashed `gos` is reclaimed instead of waiting out the deadline.
- **The native HTTP client uses happy-eyeballs**, racing all resolved addresses so an unreachable first record (commonly a filtered AAAA) falls through instead of stalling for the whole timeout.
- **`Child::kill_group` documentation corrected** on Windows (it terminates the lead process via `TerminateProcess`; there is no process-group signalling).

### CI

- **Cross-platform perf gate.** A new `perf-native` matrix job times a `gos build --release` native binary on Linux, macOS, and Windows, so an allocator/codegen regression is visible off Linux.
- **AddressSanitizer now runs on macOS** as well as Linux, giving the RC use-after-free / double-free suite portable coverage (glibc `MALLOC_CHECK_` was Linux-only).

## 0.10.0 - LLVM AOT tier completeness and soundness in the GC + fixes

Audit-driven sweep that closes 43 wiring gaps where features worked in the VM and Cranelift JIT but diverged under `gos build --release`. A new gauge - `crates/gossamer-cli/tests/llvm_aot_coverage.rs` - builds a binary per feature, runs it, and asserts stdout, so regressions surface as red bars instead of silent miscompiles.

### Cross-platform native-build fixes

- **macOS native binaries no longer crash on string literals.** Header-carrying string constants (`<{ i32 len, i8 0xA8, [N x i8] }>` with a `base+5` body alias) were emitted `unnamed_addr`, so the Mach-O backend filed the 4/8/16-byte ones into the mergeable `__literal{4,8,16}` pools. ld64 coalesces and reorders literals there and ignores the interior `.alt_entry` body symbol, so the alias resolved into the wrong literal and the runtime read a corrupt length/tag header - SIGSEGV/SIGBUS on essentially every program with a short format fragment. The backing constant is now a plain `constant` (address-significant → `__const`, stable interior symbols on every target). Guarded by a unit test that rejects `unnamed_addr` on header string constants.
- **Windows-GNU native linking.** `gos build` drives mingw's `cc` directly, so unlike a rustc-driven link it must name the libraries the runtime needs that mingw's default specs don't auto-link. `-ldl` (which mingw has no library for) is now gated to Linux only, and the Win32 import libs `ws2_32` / `bcrypt` / `advapi32` / `userenv` / `ntdll` are added on Windows. The same fix is applied to the Cranelift crate's `native.rs` link check.
- **Windows native binaries no longer corrupt `Result` / `Option` / `Vec` across the runtime boundary.** The compiled tier carries every 2-word aggregate as a scalar `i128` (`AbiType::I128`), but a by-value `i128` has no shared `extern "C"` ABI on Win64: `llc` passes it in a GP register pair and returns it there, while rustc - which compiles the runtime - passes an `i128` argument *by pointer* and returns it in a `<16 x i8>` vector register. Every `gos build` binary therefore read a corrupt discriminant/payload on Windows (wrong output, or a SIGSEGV from a payload pointer read out of the low word); SysV happens to agree, so Linux/macOS were unaffected. The LLVM tier now emits the rustc-matching shape on Windows - an `i128` argument is spilled to a 16-byte-aligned slot and passed as `ptr`, an `i128` return is called as `<16 x i8>` and `bitcast` back - at every runtime-call emission site (the two central emitters plus the inline `gos_rt_vec_push_i128` fast path that pushes a `Result`/`Option` into a `Vec`), routed through one `fat_i128_call_arg` helper so a future site cannot silently diverge, with `RuntimeEntry::llvm_declare` rendering the matching declaration. No runtime, registry, or non-Windows codegen changes; verified by comparing `llc -mtriple=x86_64-pc-windows-gnu` output against rustc's ABI, and guarded by `gossamer-abi`'s `win64_marshals_fat_i128_across_the_ffi_boundary` test. This is the complete surface: a 2-word aggregate only crosses the runtime `extern "C"` boundary as a machine value in the LLVM AOT tier (now fixed). The bytecode interpreter calls the runtime as in-process Rust with no FFI boundary, and the Cranelift JIT does not compile `i128`-shaped bodies at all (no `JitKind::I128`; Cranelift panics on an `i128` argument/return without `enable_llvm_abi_extensions`), so such bodies fall back to the interpreter - correct on every platform. `gos` of a fat program (`result::default_with`, `hex::decode`) produces correct output on Win64 through that path, JIT forced on or off.
- **Runtime staticlib is published atomically.** `gossamer-cli`'s build script copies the ~300 MB `libgossamer_runtime.a` into `target/<profile>/` for non-cargo linkers (`gos build`, the Cranelift `native.rs` link tests). The copy was a plain `fs::copy`, which truncates the destination and streams the bytes; because the script re-runs whenever a `GOS_*` env var changes (the diagnose CI step sets several), a parallel test reading the archive mid-write hit `ld: failed to set dynamic section sizes: file truncated`. The publish now copies to a per-pid temp file and `rename`s it into place, so a reader always sees a complete archive. (Surfaced because the `native.rs` link helper no longer silently skips link failures - see below.)
- **Native-build test diagnostics.** Link errors on a supported platform now fail loudly with the full `cc` stderr instead of silently skipping (a silent skip hid the `-ldl` break); `GOS_LINK_VERBOSE` echoes the resolved linker line + libraries; `GOS_KEEP_BUILD_ARTIFACTS` / failing three-tier harnesses preserve sources, objects and binaries for CI artifact upload; and exit codes are rendered as their cause (`killed by signal 11 (SIGSEGV)`, `exit code 0xC0000005 (STATUS_ACCESS_VIOLATION)`) rather than an opaque number.

### Compiled-tier reference counting replaces the tracing GC

The compiled tiers (Cranelift JIT, LLVM AOT) now manage recursive heap-enum lifetime with intrusive reference counting - matching the interpreter's `Arc`-payload semantics - instead of the raw-pointer tracing collector, which was unsound under `opt -O3` (live roots are not precisely discoverable) and leaked or crashed on tree-shaped heaps. Soundness is verified across aliasing, struct-embedding, return-of-argument, container, and payload-variant cases under glibc's `MALLOC_CHECK_=3`.

- **Intrusive RC runtime** (`gos_rt_rc_alloc` / `_retain` / `_release`, `c_abi/rc.rs`): every heap object carries a strong count plus a flat `[i64]` child-layout descriptor; release is iterative so deep structures cannot overflow the runtime stack. User enum constructors allocate through it, and a per-variant descriptor is emitted once as a module constant in both backends.
- **Balanced retain/release insertion** (`gossamer-mir`): retain on every aliasing copy / field store / aggregate / container insert, release every owned local at scope exit, with move elision so the construct-and-return pattern costs zero refcount traffic. Interior borrows (match bindings, accessor results) are never released.
- **Per-call tracing-GC instrumentation removed.** The shadow-stack save/push/restore and safepoint hooks previously emitted on every function call are gone (the collector they fed is superseded by RC). Hot leaf-math loops return to native parity after a large release-mode regression, and recursive-enum allocation workloads run several times faster.
- **Two latent optimizer miscompiles fixed** (`gossamer-mir::opt`): `const_value_of` and `copy_propagate` both treated a local's first constant assignment as its value, ignoring a later reassignment - a use after the reassignment could fold a live heap pointer to null.
- **Incremental object cache now keyed by compiler fingerprint.** The per-body LLVM object cache hashed only the MIR, target, and opt profile - so a rebuilt compiler that emits different IR for identical MIR (e.g. after the tracing-GC removal) silently reused stale objects, surfacing as link failures against removed runtime symbols or as "fixed-but-still-slow" binaries. The key now mixes the package version and the compiler executable's size + mtime.
- **Dead tracing-GC machinery removed.** The raw-pointer collector, shadow-stack roots, safepoint/write-barrier shims, allocation registry, and per-call instrumentation are deleted from the runtime, ABI registry, and codegen; the aggregate allocators (`gos_rt_aggr_alloc`/`_free`) and the deterministic drop pass remain. `std::runtime::gc_collect()` is retained as a no-op (RC reclaims automatically). A `--release` performance canary (`tests/perf_canary.rs`) guards against per-call-overhead regressions in the hot scalar path.

### RC for container / Result-nested enums

Four drop-pass / RC bugs that corrupted or miscompiled recursive enums carrying `Vec`, tuple, and `Result` payloads - the shape of a JSON-value tree - are fixed. Covered by `crates/gossamer-cli/tests/rc_nested_containers.rs`.

- **Loop element borrows are no longer released.** A `for x in xs` element loaded through a terminator-position `gos_load` (block boundary, not the `CallIntrinsic` form) was treated as owned and released each iteration, freeing the container's elements. `gos_load` / `gos_store` in terminator position are now recognised as borrows.
- **A `Vec` stored into a returned enum survives.** The drop pass freed a `Vec` local at return even after it was stored into a returned `J::Arr(v)`; the escape analysis now follows `gos_store(obj, off, val)` into an escaping object.
- **Deep container nesting composes.** `outer.push(J::Arr(inner))` then `J::Arr(outer)` lost the innermost `Vec` because the `vec_push` and `gos_store` escape rules ran in separate passes; they are now one fixpoint.

### By-value `Result` / `Option` and inline enum payloads

`Result<T, E>` and `Option<T>` are now a 2-word by-value `i128` (`[disc, payload]`) rather than a heap-boxed `*mut GosResult`. The box was allocated on every `Ok` / `Err` / `Some` / `None` and never reclaimed - an unbounded leak on every `?`. `ast` / `json` / `gc` workloads are unaffected and output is bit-identical across all tiers.

- **2-word representation** (`AbiType::I128`; `render_ty` and the Cranelift layout map the sentinel ADTs to `i128`; `pack_result` / `gos_rt_result_disc` / `gos_rt_result_payload` in `c_abi/vec.rs`): discriminant in the low word, payload (a scalar inline, or a pointer to a larger value) in the high word. The `?` desugar, `match`, field access, and the `result::*` / `option::*` combinators read and build it directly; `is_rc_managed` reports these as values, never RC pointers.
- **16-byte `Vec` / array elements.** A by-value `Result` / `Option` element occupies two slots: `slot_count` / `type_slot_bytes` / `aggr_size_bytes` report two slots for the sentinels, with `gos_rt_vec_push_i128` / `gos_rt_vec_get_i128` and matching push / index / for-loop element reads. `regex::captures` / `captures_all` (returning `Vec<Vec<Option<String>>>`) round-trip bit-identically across the VM, Cranelift, and LLVM tiers.
- **Inline enum payloads.** A user enum whose every variant has at most one field that fits in a single 8-byte slot (scalar / `String` / `Vec` / map / handle - the shape of a JSON-value enum) uses the same 2-word by-value representation: construction packs the discriminant and field with no heap node, `match` reads the discriminant from the low word, and the single field is the high word. Multi-field variants (e.g. a tree node) keep the heap-node representation.
- **Payload-less variant singleton.** A no-field variant (`Tree::Leaf`, `JsonVal::Null`, …) returns one process-pinned, globally-allocated per-discriminant node instead of allocating a fresh node per construction (the node is shared and never mutated).

### `for x in vec` single-slot element read

A `for`-loop over a `Vec` of single-slot, non-float elements (i64 / bool / `String` / handle) reads each element with one `gos_rt_vec_get_i64` instead of `gos_rt_vec_get_ptr` + `gos_load` (two runtime calls), halving the per-element call overhead on adjacency-style iteration (graph-bfs).

### HTTP server: per-request memory leak fixed

The compiled HTTP server leaked every request's `Ok(Response)` result box. Per-request reclamation had relied on a per-worker arena reset (`gos_rt_gc_reset`) that became a no-op when the bump arena was retired, and on the tracing GC that the reference-counting migration removed - so `gos_rt_result_new`'s `Box::into_raw` was never freed. Under load the server grew unboundedly; `drop_handler_result` now frees the result box after the response is written. 

### Lenient out-of-bounds indexing parity (VM matches compiled)

The interpreter aborted on an out-of-range index while the compiled tiers return the element zero value; `gos` now matches `gos build` (any index outside `[0, len)` yields the zero value, no panic), so the two tiers are bit-identical on out-of-bounds access.

### Optimizer attributes on runtime declarations

Every `gos_rt_*` LLVM declaration now carries `nounwind` (correct: an `extern "C"` boundary aborts rather than unwinds, so the call never throws), and an audited set of pure getters (`vec_get`, `vec_len`, `arr_len`, `str_len`, `str_byte_at`, `str_eq`, `heap_i64_get`) additionally carries `memory(argmem: read)`. Without these, LLVM treated every runtime call as a potential exception edge and a full memory clobber, blocking reordering, hoisting, and CSE of surrounding loads/stores; the attributes let `opt` move loop-invariant runtime reads out of loops.

### Reference-counting memory-footprint fixes

Three coordinated fixes cut compiled-tier RAM on heap-heavy workloads. A named local bound to a recursive heap value and rebuilt each loop iteration was leaking every iteration's value until the function returned; a pathological loop that should hold ~11 MB held 863 MB. Covered by a new named-binding-loop RSS regression test (the prior test only exercised the temporary shape, which already released).

- **Release before reassignment, not only at return.** Owned reference-counted locals are now released before *any* reassignment (including the loop back-edge), not just before a fresh allocation. A `let t = build(d)` rebuilt each iteration frees the previous tree instead of accumulating all of them. The entry zero-init keeps the first release null-safe.
- **16-byte object header.** `RcHeader` shrank from 24 to 16 bytes (`strong` and `size` are now `u32` - 4 billion live refs / 4 GiB objects are unreachable ceilings), so a `Node(i64, Box, Box)` is 40 bytes instead of 48.
- **Byte-budgeted recycling pool.** The thread-local free-list is now capped by a 4 MiB-per-class byte budget instead of a flat 65k-block count, so a large size class can no longer pin tens of MiB of cached blocks.

### Container element ownership + per-iteration `Vec` reclaim

A string or nested container stored in a `Vec` no longer leaks, and a `Vec` rebuilt each loop iteration is reclaimed instead of accumulating.

- **No per-push element clone.** `gos_rt_vec_push` copied each STRING element into a vec-owned buffer (a value-semantics relic), while the drop pass separately retained the caller's original - so that original leaked once per push. Elements are now held by reference: the compile-time RC (retain at insert, `elem_kind` deep-free at container drop) owns each exactly once, the same model as struct fields. `string_in_vec` / `nested_vec_string` drop from O(n) live strings to O(1).
- **Loop-local `Vec` freed per iteration.** A `Vec` constructed in a loop body was freed only at function return, leaking every prior iteration's container and its elements. The drop pass now frees the previous value before each constructor reassignment (null-safe via an entry zero-init) and at each return, conservatively skipping any container that escapes into another container or the return value. A deterministic per-family allocation ledger (`c_abi/ledger.rs`, `GOS_LEAK_LEDGER`, unix) backs the leak-shape gate.
- **`HashMap` insert releases its inbound strings.** `gos_rt_map_insert_str_*` / `_i64_str` copy the key/value bytes into the map's own storage, so the consuming-call contract leaves the caller's `format!(...)` key/value as a leaked temporary. The runtime now releases each inbound gos-string after copying (rc-aware + tag-checked, so a moved temp is freed, a shared string is only decremented, and a literal is skipped).
- **Fresh string producers are owned.** `str_repeat` / `slice` / `substring` / `trim*` / `replace*` / `pad_*` / `to_upper`/`to_lower`/`to_title` return a freshly allocated owned `String`, so a standalone transient (`let big = strings::repeat(…); use(&big)`) is released at scope instead of leaking. A returned producer result is exempted (it flows to the caller). The substring-retention leak benchmark goes from ~290 MB to flat ~0.6 MB. Deliberately excludes `concat` (in-place-aliasing in `s += …`) and `Result`/`Option` payload extraction.

- **Loop-local `HashMap` / `HashSet` reclaimed.** `let m = HashMap::new()` lowered to `tmp = map_new(); m = Copy(tmp)`; the copy pinned the constructor result as aliased, so the reuse pass never reclaimed a loop-local map. Container constructors (and `Some` / `Ok` / `Err`) now write the binding directly - no copy, no alias - so a loop-local map / set is freed per iteration like a `Vec`. A map passed to a user function stays safe via the existing escape disqualification.
- **By-value enum payload extraction is a move.** A `String` moved out of a consumed `Result` / `Option` (`let s = f()?`, `r.unwrap()`, `match o { Some(s) => … }`) transfers the enum's single owning reference to the binding instead of retaining a second, so the binding releases it exactly once. When the extracted value is instead stored into an aggregate - the synthesized `from_json` parses a field `String` and places it in the result struct through copy temporaries - the retain is load-bearing and kept, detected by propagating "stored into an aggregate" transitively backward through copy edges. An aliased enum (`let o2 = o; match o2; match o`) is conservatively not owned (leak-not-double-free), and autoderive `from_json` / `to_json` round-trips clean under `MALLOC_CHECK_=3`.
- **`?` / match payload typing.** The extracted payload type is recovered from the scrutinee enum's substitution (the declared variant field type is the generic default, often `i64`), and concrete types are propagated forward through `Copy` chains, so a `?` extraction copied into an otherwise-`Var` binding is recognised as RC-managed and released.
- **Leak ledger no longer counts region-managed strings.** A string allocated inside an arena region is reclaimed wholesale at `region_pop` (and skipped by `gos_rt_str_free`), so an unmatched `str_inc` made the `GOS_LEAK_LEDGER` gauge report a false positive on region-heavy loops; region strings are no longer counted in the per-string gauge (the memory is bounded by the region).

### Arena regions wrap only allocating loops

- A loop body is wrapped in an arena region (`region_push` / `region_pop`) only when it actually allocates a heap value. A purely-scalar inner loop (a counter scan, byte stores) previously paid two region calls every iteration for nothing; eligibility now also requires a heap-allocating call or constructor in the body. Allocating loops stay regioned and bounded.
- A tuple field read out of a fixed array (`table[j].1`) lowers to a single combined index+field projection instead of materialising the whole tuple to extract one field, and `buf.set_byte(i, x)` lowers to an inlined branchless bounds-guarded store in the LLVM tier instead of a per-byte runtime call.

### Length-carrying strings - O(1) `len`/`slice`

Compiled-tier strings now store their byte length in the allocation header, so length and slicing are O(1) instead of `strlen`-per-call. A recursive-descent parser that slices a large input at growing offsets was O(n^2); **json-serde drops from 167s to 0.54s at N=50000** (now linear, output bit-identical to the Rust reference).

- Heap strings (`format!`, `slice`, file reads, every `alloc_cstring` caller) use the length-carrying builder layout, so `gos_rt_str_len` reads the stored length at `ptr[-5]`; foreign pointers fall back to `strlen`.
- `gos_rt_str_slice` bounds-checks against the O(1) length and copies the range directly - the safe out-of-bounds `Err` contract is preserved (no UB fast path).
- LLVM string literals emit a length-carrying header (`<{ i32 len, i8 tag, bytes }>`) with a global alias at the body, so literal references are unchanged while their length is O(1) too.
- C interop is unchanged: the body pointer still points at NUL-terminated bytes (the length header sits before it).

### `gos clean` removes build artifacts + caches

`gos clean` now also removes the project `target/` directory and the
per-project `.gos-cache` incremental IR-object cache (previously it dropped
only the frontend cache). `--dry-run` reports without deleting; `--vendor`
additionally drops `vendor/`. Idempotent - absent targets are noted and
skipped.

### Recycling RC allocator (thread-local slab)

`gos_rt_rc_alloc` / release now route small RC objects through a per-thread, lock-free size-class free-list that recycles freed blocks instead of round-tripping through libc `malloc`/`free` on every node. Allocation-heavy workloads (recursive-enum trees) roughly halve: the gc-trees stress test drops from ~20s to ~12s. The pool returns surplus blocks to the OS at a per-class cap and frees its cache on thread exit (so the HTTP server's per-connection threads don't leak); `GOS_RC_NO_POOL=1` disables it so `MALLOC_CHECK_` retains full double-free detection in the soundness tests.

### `String::byte_at` interpreter binding

`s.byte_at(i) -> i64` was wired through the compiled tiers but unbound on the interpreter. It is now a registered `String` method on every tier (the UTF-8 byte at `i`, or 0 out of range), matching `gos_rt_str_byte_at`.

### Generic-struct field types + `impl` method `self` typing

Three coordinated typechecker fixes ground inference results that previously leaked unresolved `Var`s into lowering, where the compiled tier defaulted them to i64/ptr and mis-stringified values.

- **Unsuffixed float literals default to `f64`.** `InferCtxt` gained a `float_literal` var flavour (mirroring the integer-constrained flavour) plus `default_unresolved_float_vars`. A bare `3.0` fed into a generic position (`Triple { third: 3.0 }`) previously left its inference var unbound; the field then printed the value's IEEE-754 bit pattern through `gos_rt_concat_i64` (`4613937818241073152` instead of `3`). Float literals now take their use-site width when constrained and fall back to `f64` otherwise.
- **`deep_resolve` recurses into `Adt` substs.** The end-of-typecheck zonk only grounded `FnPtr` / `FnTrait` sigs, so a `Triple<?, ?, ?>` whose vars unified to `<i64, String, f64>` stayed recorded with unresolved substs. It now resolves each `Adt` type argument, so a generic struct's field access substitutes the concrete type.
- **`impl` method `self` binds to the concrete `Self` type.** The receiver was bound to a fresh inference var, so `self.field` reads left the field type unresolved - a `for x in self.items` over a `[String]` field bound `x` at the i64 default (the auto-derived `to_json` serialised a `[String]` field as integer pointers: `["2100555", …]`). `self` now binds to the impl's `Self` (wrapped in `&` / `&mut` for `&self` / `&mut self` receivers).

### Native bytecode-VM `match` compilation

The bytecode VM (`gos`) now lowers `match` expressions to a native test-and-branch chain instead of routing every arm evaluation through the bundled tree-walker via `Op::EvalDeferred`. Across the example suite the walker-fallback count drops sharply (`shapes.gos` 20 → 0, `temperature.gos` 18 → 0, `json_structs.gos` 24 → 4).

- **Three new opcodes** - `VariantIs` (enum/tuple-struct name + arity test), `VariantField` (positional payload extract), and `StructIs` (struct-name test) - back the pattern tests; literals compare via `Eq`, ranges via `Ge`/`Le`/`Lt`, tuple/struct fields project via the existing `TupleIndex` / `FieldGet` ops.
- **`compile_match` + `emit_pattern_test`** lower every native-expressible pattern shape: wildcard, binding, literal, range, enum variant (with nested payload patterns), tuple (including a `..` rest), struct (with field-shorthand binding), `&`-ref, `@`-binding, and or-patterns of non-binding alternatives. Guards compile inline after the pattern test.
- **Fallback preserved** - an or-pattern that introduces bindings still routes the whole `match` through the walker, so semantics stay correct while the common 95% runs natively. (Closures, `go`, and `select` remain walker-evaluated; the walker is not yet deleted.)
- **`get()` bare-name router** - exercising `match` scrutinees natively exposed a latent dispatch collision: `install_module("json", …)` registered `("get", builtin_json_get)` after the HashMap getter, so a natively-evaluated `m.get(&k)` returned `None` and `match m.get(&k) { Some(v) => … }` always took the `None` arm. A receiver-dispatching `builtin_get_router` (mirroring the `keys`/`values` routers) sends `Map`/`IntMap` receivers to the map getter and struct/json receivers to the json getter.
- **Compiled-tier tuple-match binding** - `match` on a tuple whose element types inference left loose (`let pair = (10, "hi")`) bound each element through a pointer-shaped local, so the `println!` arg dispatcher routed the `i64` element through `gos_rt_concat_str` and strlen'd the integer → segfault. The MIR tuple-pattern lowering now recovers each element type from the sub-pattern when the tuple's recorded type is unresolved.

### Free-fn dispatch wired through MIR

- `strconv::parse_i64` / `parse_f64` / `parse_bool` / `parse_u64` / `atoi` / `format_i64` / `format_f64` / `format_bool` / `itoa` - new `gos_rt_strconv_*` shims (`c_abi/strconv.rs`) with Result-shaped payloads where the VM returns Result.
- `strings::trim` / `trim_start` / `trim_end` / `split` / `to_upper` / `to_lower` / `contains` / `replace` / `starts_with` / `ends_with` / `lines` / `find` / `repeat`.
- `math::tan` / `asin` / `acos` / `atan` / `atan2` / `sinh` / `cosh` / `tanh` / `log2` / `log10` / `cbrt` / `round` / `exp2` / `fmod` / `hypot` / `copysign` / `dim`.
- `path::parent` / `stem` / `file_name` - new Option-returning shims.
- `env::set_var`, `env::program_name` (registry entry was missing), `crypto::rand::bytes` (new `getrandom`-backed shim), `fs::metadata`, `time::Duration::as_millis` / `from_micros` / `as_secs` / `as_micros`, `sync::AtomicBool::new` / `sync::AtomicU64::new` (alias to AtomicI64).
- `encoding::xml::escape`, `encoding::base32::encode` / `encode_string` / `decode_string`, `encoding::base64::encode` / `decode`, `encoding::hex::encode` / `decode`, `html::escape` / `unescape`, `compress::flate::compress` / `decompress`, `compress::zlib::compress` / `decompress`, `crypto::hmac::sha256_mac`, `result::default_with` - previously emitted an undefined `@module::fn` reference at the `opt` stage of `gos build`. New `gos_rt_*` shims (`c_abi/encoding.rs`, plus flate/zlib in `c_abi/gzip.rs`, hmac in `c_abi/crypto.rs`), MIR dispatch arms, and ABI-registry entries lower them across the compiled tiers. A `Vec<u8>` is stored i64-per-element (each byte zero-extended to an 8-byte slot), and the byte readers/builders in the new shims respect that. Acceptance gate: `crates/gossamer-cli/tests/stdlib_lowering.rs` builds + runs a probe per function.

### More VM-only stdlib surface wired through MIR

A reverse audit (interp-registered builtins with no compiled-tier lowering) found a large further set of `module::fn` calls that ran under `gos` but emitted an undefined `@module::fn` symbol at the `opt` stage of `gos build`. The `dispatch_parity` test only checks the runtime→codegen direction, so this whole class was ungated. Each function below now has a `gos_rt_*` shim, an ABI-registry entry, and a MIR dispatch arm, and is exercised by a `feature-testing-examples/` fixture that asserts bit-identical stdout across VM / Cranelift / LLVM.

- **`strings`** - `splitn`, `split_whitespace`, `fields`, `replacen`, `to_title`, `trim_matches`, `pad_left`, `pad_right`, `contains_rune`, `contains_any`, `equal_fold`, `index_rune`, `index_any`, `last_index_any`, `strip_prefix`, `strip_suffix`.
- **`path`** - `clean`, `normalize`, `is_absolute`, `has_prefix`, `extension` (aliases the existing `ext` Option shim).
- **`time`** - `sleep`, `now`, `unix_ms`, `now_nanos`, `monotonic_ms`, `monotonic_nanos`, `since_ms` (monotonic shims already existed; these route the language-level calls plus new epoch-nanos / since shims).
- **`hash`** - `crc32::{checksum, checksum_string, update}`, `adler32::{checksum, checksum_string, update}`, `fnv::{hash32, hash64, hash_string}` (new `c_abi/hash.rs`).
- **`math::bits`** - scalar primitives `count_ones`, `count_zeros`, `leading_zeros`, `trailing_zeros`, `reverse_bits`, `reverse_bytes`, `len`, `rotate_left`, `rotate_right`.
- **`os` / `fs`** - `copy` (Result<i64>), `canonicalize` (Result<String>).
- **`crypto::subtle::constant_time_eq`** - length-aware constant-time byte compare.
- **`encoding::ascii85`** - `encode`, `decode`.
- **`encoding::utf16`** - `is_surrogate`, `rune_len`, `decode_surrogate_pair` (Option<char>), `encode_string` ([u16]), `decode_to_string`. The interp registration was also fixed to bind the canonical `encoding::utf16::*` path (it previously only bound the bare `utf16::*` form, so `use std::encoding; encoding::utf16::…` failed in the VM too).
- **`encoding::binary`** - `put_u16/u32/u64_be/le` ([u8]), `get_u16/u32/u64_be/le` (Result<i64>), `uvarint` / `varint` (Result<(i64, i64)>).
- **`encoding::csv`** - `parse_line` ([String]), `read` (Result<[[String]]>), `write` (String). Exercises the nested `Vec<Vec<String>>` representation across the by-value-aggregate ABI.
- **`bufio`** - `read_to_string` (Result<String>), `read_lines_of` (Result<[String]>), `split_whitespace`.
- **`net`** - `resolve` / `lookup` (Result<[String]>).

The carrying `math::bits::{add, sub, mul, div}` and the `utf8::{decode_rune, decode_rune_in_string, decode_last_rune, decode_last_rune_in_string}` family return by-value tuples (`(i64, i64)` / `(char, i64)`); `utf8::append_rune` returns `[u8]`. These exercise the compiled-tier by-value-aggregate ABI - a runtime helper returns a GC-allocated multi-slot heap buffer that the caller memcpys into its destination, the same shape user-defined tuple/struct returns already use across both backends.

### Struct-returning stdlib functions via injected real-struct wrappers

The last VM-only class was stdlib functions that build or return a *named struct* (`pem::Block`, `x509::CertInfo`, `tar::TarEntry`, `zip::ZipEntry`). Rather than a fragile sentinel-DefId opaque handle (which disagrees with the multi-slot inline layout the compiled tier gives real structs), each is wired through the serde-autoderive precedent: `gossamer-parse` injects real Gossamer `struct` + wrapper-fn source, and a `VisitorMut` rewrites the public call/type sites (`pem::decode`, `x509::CertInfo`, …) to the mangled wrappers. Each wrapper calls a leaf intrinsic that returns the proven tuple / `[u8]` ABI shapes; the wrapper folds the tuple into the real struct, which then constructs, indexes, and field-accesses identically on every tier.

- **`encoding::pem`** - `decode` / `decode_all` / `encode` over a real `Block { block_type, bytes }`. Leaf intrinsics `gos_rt_pem_decode_raw` (`Result<(String, [u8])>`), `gos_rt_pem_decode_all_raw` (`Result<[(String, [u8])]>`), `gos_rt_pem_encode_raw`.
- **`crypto::x509::parse_pem`** - `Result<CertInfo, Error>` over a real 7-field struct, via a single `gos_rt_x509_parse_pem_raw` leaf returning a 7-slot `(subject, issuer, serial, not_before_unix, not_after_unix, san_dns, sha256)` tuple. The runtime shim reuses `x509-parser` + `sha2` so the compiled tier matches the VM byte-for-byte.
- **`archive::tar` / `archive::zip`** - `read` returns `[TarEntry]` / `[ZipEntry]` (each `{ name, data, is_dir }`) via a `[(String, [u8], bool)]` tuple-vec leaf; `write([(String, [u8])])` returns `Result<[u8]>` directly (no struct). Runtime shims use the `tar` / `zip` crates.

Three general codegen fixes fell out of this work and benefit all user structs, not just the stdlib wrappers:

- **`[u8]` / `[T]` field method dispatch** - a struct extracted from a `Result` (`match Ok(q) => q.bytes.len()`) lost contact with its field types, so `.len()` on a `[u8]` field dispatched to `strlen` and read the i64-per-element Vec as a C string (returning 1, or crashing on a misaligned pointer). The method-call lowering now recovers the field's declared type from the parent struct's `Adt` def - ground truth - instead of the wrongly-resolved HIR type.
- **Array-literal struct fields coerce to heap Vec** - `Q { bytes: [1, 2, 3] }` where `bytes: [u8]` stored the 3-slot inline array straight into the 1-slot Vec field, overflowing the aggregate. The struct-literal lowering coerces an array-literal value to a `GosVec` when the field is declared `[T]` / slice.
- **Field-access type recovery** prefers the struct's declared field type whenever the receiver is an `Adt` with known fields, not only when the HIR type is an unresolved `Var`.
- **Array/tuple-literal arguments re-type to the parameter.** A literal argument is re-recorded against the callee parameter type, so a nested `[1, 2, 3]` byte array inside a `(String, [u8])` tuple inside a `[(String, [u8])]` parameter (the `archive::tar`/`zip` `write` shape) is typed as a heap Vec at every level rather than a fixed `[i64; N]` - the compiled tier then lays out the same heap structure the runtime shim reads. A per-body pre-scan extends this through a `let` binding (`let files = […]; tar::write(files)`): the binding whose value flows into such a call is re-typed up front, the backward inference the single-pass checker can't otherwise reach.

### Method dispatch fallthroughs

- `HashMap::contains` aliases `contains_key`; `BTreeMap::get` / `contains` / `contains_key` - three new btmap shims.

### Result<f64> bit-pattern preservation

- `Ok(f64)` packs via `gos_rt_result_new_f64` (`to_bits`) and unpacks via `gos_rt_result_payload_f64` (bit reinterpretation). The prior path went through `fptosi`/`sitofp` and silently truncated `3.5` to `3`.

### Closure ABI through unified Fn trampoline

- `gossamer-hir::lift_closures` now pins unresolved (`Var`/`Error`/`Param`) closure param + return types to `i64` after the lift pass. LLVM was emitting `__closure_N(ptr) -> ptr` for `|n| println!("{}", n)`-style closures while the trampoline called them as `(i64) -> i64`; the ABI mismatch segfaulted inside `iter::for_each` / `option::map` / `result::map`. New `gos_rt_option_map_i64` / `gos_rt_result_map_i64` complete the map surface for Some/Ok payloads.

### Silent miscompiles closed

- `let mut xs = [1, 2, 3]; xs.push(4)` - MIR's let-lowering promotes `mut` array-literal bindings to `Vec<T>` so `.push` / `.sort` / `.iter` don't write through a stack `[i64; N]` interpreted as a `GosVec` header.
- `gos_rt_set_args` captures `argv[0]` whenever `argc >= 1` (was gated behind `argc > 1`), so `env::program_name()` returns the binary path even when run with no user args.
- `gos_rt_crypto_rand_bytes` writes the requested length into the `GosVec` header after filling the buffer.
- `regex::captures_all` / `captures` build canonical `Option<String>` capture groups. The runtime pushed a bare c-string pointer (or 0) per group, but each group's source type is `Option<String>`; when the element typed as a concrete `Option<String>` (e.g. through a function whose declared return is `[[Option<String>]]`), the compiled-tier `match group { Some(k) => …, None => … }` read the tagged-union discriminant (`gos_rt_result_disc`) off the pointer and saw a c-string's first bytes as garbage, so the match fell through and produced no output. The runtime now pushes `gos_rt_result_new(disc, payload)` Options and the MIR pins the result element to `Option<String>` (`captures_all` → `Vec<Vec<Option<String>>>`, `captures` → `Option<Vec<Option<String>>>`, and the `for row in captures_all(…)` element to `Vec<Option<String>>`).

### Coverage gauge

- `tests/llvm_aot_coverage.rs` - 43 round-tripped tests, 0 ignored. Each test pins a behaviour the audit found broken; the suite is the regression gate for future LLVM-tier work.

### `&mut T` deref-assign and `&mut self` field mutation

Three coordinated fixes close a class of LLVM AOT segfaults / silent miscompiles where `&mut scalar` was passed as an i64-as-ptr and `*s = expr` was silently dropped.

- **`*place = expr` (deref-assign) routes through a Place with `Projection::Deref`** - `gossamer_mir::lower::builder::expr::lower_place_expr` gained a `HirUnaryOp::Deref` arm that appends a `Projection::Deref` step. Previously the match defaulted to `None`, so `lower_assign` silently returned without emitting any store, and the program silently dropped the entire assignment.
- **LLVM `lower_place_address` skips its prefix auto-deref when the first projection is itself `Deref`** - the auto-deref exists for the common shape `let r: &T = &x; r.field` (loads the local's pointer slot once before walking field offsets). When `*r = expr` arrives with `Place { local: r, projection: [Deref] }`, both the auto-deref and the explicit `Deref` would fire - the second load reads garbage at the pointee's first 8 bytes. The new `skip_auto_deref` check on `place.projection.first()` keeps single-level pointer semantics correct for both shapes.
- **`&mut`-on-place-of-scalar emits `Rvalue::Ref`** - `lower_unary` previously returned `Some(inner)` for every `RefShared` / `RefMut`. For aggregates (Vec/String/struct/opaque-handle Adts whose locals already hold a pointer) that's correct. For `&mut` on a scalar place (`&mut state`, `&mut p.field`, `&mut arr[i]` where the element is `i64`/`f64`/`bool`/`char`), the caller used to hand the callee the **value as a pointer**, segfaulting on the first deref. The lowerer now narrows to the `&mut` + scalar + genuine-place shape and emits `Rvalue::Ref { mutable: true, place }` so backends compute a real slot address. Shared `&` on scalars and `&` on literals keep their historical value-passthrough so existing dispatch (e.g. `map.get(&k)` → `gos_rt_map_get_i64(m, k_value)`) continues to work.
- **Cranelift `Rvalue::Ref` for bare scalar locals materialises a stack slot** - when the address is asked for a local that lives in an SSA `Variable` (the common cranelift shape for `i64`/`f64`/`bool`/`char`), the handler now allocates an 8-byte stack slot, stores the current value, and returns `stack_addr`. The LLVM tier didn't need this because alloca-backed locals always have an address; cranelift required the explicit promotion for the `&mut state` path to produce a real pointer.
- **Net effect** - `fn lcg(s: &mut i64) { *s = *s * K + C }` now runs correctly under both `gos build` and `gos build --release` instead of segfaulting; `impl P { fn advance(&mut self) { self.pos += 1 } }` writes back through the pointer. The bytecode-VM / walker tier still has the long-standing `&mut self` writeback gap on field mutation.

### Multi-dim fixed-array indexing

- **`lower_place_address` advances `current_ty` after every `Index` step** - when projecting `arr[i][j]` over `[[T; A]; B]`, the LLVM lowerer previously left `current_ty` pinned at the outer array type and reset `stride_slots` to 1 after the first index. The second index then used the outer array's bounds (panic with `len is 2 but index is 2` after a clean exit from a `while s < 2` loop), and the stride was wrong for the element width - corrupting the data. The Index arm now matches the Field arm and walks into the element type, recomputing `stride_slots = elem_slots(elem_ty)`. The chess-engine `make_zobrist`-style writes over `[[[i64; 64]; 6]; 2]` round-trip cleanly across all tiers.

### `env::args()` empty-iteration safety

- **`gos_rt_set_args` materialises an empty `GosVec` when `argc <= 1`** - previously the no-user-arg branch stored a null pointer into `ARGS_VEC`, and any iteration over `env::args()` (`for a in args { ... }`) dereferenced the null header and segfaulted. The header is now a zero-length stack-stable `GosVec` with `ptr = null`, `len = 0`, `cap = 0`, so the iterator's `header.ptr + 0 * elem_bytes` walk is a clean zero-trip.

### `xs.pop()` on typed-storage arrays

- **`builtin_pop` handles `Value::IntArray` and `Value::FloatVec`** - the receiver dispatch previously only covered `Value::Array`. A `let mut xs: [i64] = [..]` lands as `Value::IntArray`, fell into the `_ => empty_array` fallback, and the writeback then moved the empty result into `xs` - clobbering the entire vector. Both typed-storage variants now shrink by one element instead of being zeroed out.

### Interpreter RAM - shared prelude, interned identifiers, end-of-load compaction

- **Process-shared prelude `Arc<FxHashMap<&'static str, Global>>`** - `builtins::prelude_globals()` builds the ~330-entry built-in dispatch table once via `OnceLock`; every `Vm::new` and `Vm::with_globals` `Arc::clones` it. New `Vm::lookup_global` / `lookup_global_ref` two-tier helpers consult the per-Vm overlay first, then the shared prelude on miss. Goroutine-heavy programs no longer pay per-Vm prelude duplication. Every `Op::Call` / `Op::MethodCall` / `Op::LoadGlobal` / `Op::SpawnMethod` dispatch site now routes through `lookup_global*`.
- **`Vm.globals` keyed by `&'static str`** - `Arc<HashMap<String, Global>>` → `Arc<FxHashMap<&'static str, Global>>`. Dynamic qualified keys (`format!("{prefix}::{name}")`) intern through `value::intern_type_name`. Eliminates ~330 per-Vm `String` heap allocations and the `to_string()` calls that fed them.
- **`FnChunk::name: &'static str`** - interned at chunk construction (in `compile_fn`). `FnBuilder::name` follows. Recursive programs no longer allocate one `String` per call-stack frame.
- **`Vm.call_stack: RefCell<Vec<&'static str>>`** - interned chunk-name push instead of `String::clone` per `apply` entry. `call_stack_snapshot` still returns `Vec<String>` for API stability.
- **Interner pools migrated to `FxHashSet<&'static str>`** - the process-global `value::intern_type_name` and the per-thread `vm::intern_type_name` / `vm::intern_qualified` swapped from `Vec<(String, &'static str)>` linear scan to a hash-set of leaked `&'static str`. Lookups stay O(1) past the small-program range; hits no longer allocate a probe `String`.
- **`FnBuilder::finish()` folds in `compact()`** - every chunk-construction path now `shrink_to_fit`s its Vec storage automatically; new code that produces a chunk through `finish` cannot accidentally skip the compaction.
- **`Vm::load` ends with `globals.shrink_to_fit()`** - releases hashbrown's growth-by-doubling slack on the overlay once every item is registered.
- **`release_jit_prelude` extended** - drops `mir_bodies` + `tcx_snapshot` and now also `shrink_to_fit`s `chunk_state_arena` + `chunk_state_map` so the post-`call` Vm's RSS reflects steady state while goroutines drain.

### Short-circuit `&&` / `||` in the compiled tier

- **`lower_binary` branches on the LHS for logical AND/OR** - the MIR lowerer previously called `lower_expr` on both sides up front. Any guarded RHS (`while j > 0 && arr[j - 1] < x`) evaluated the bounds-violating index unconditionally and panicked with `index is -1` once the LHS guard kicked in. The lowering now emits a small branch lattice: LHS → switch → (short-circuit constant) or (eval RHS) → merge. VM tier was already correct via the walker's expression evaluator; this brings the compiled tier in line.

### `HashMap` bare-name dispatch router

- **`builtin_keys_router` / `builtin_values_router`** - `install_module("json", …)`'s unconditional bare-name push registered `("keys", builtin_json_keys)` AFTER the HashMap surface's `("keys", builtin_map_keys)`. The later json push silently overrode the bare-name registry, so every `m.keys()` on a HashMap dispatched to the JSON helper which returns `None` for non-Struct receivers - surfacing as `ks.len() == 0` even with multiple inserts. A small router dispatches on the Value variant so both surfaces work without depending on registration order.

### Array literal → `Vec` / `Slice` return coercion

- **`Return` lowering coerces `Array<T; N>` to `Vec<T>` when the declared return is `Vec(elem)` / `Slice(elem)`** - `fn f() -> [String] { return ["a", "b"] }` previously lowered the literal as a flat stack-aggregate that the caller dereferenced as a `*mut GosVec` (len read as garbage bits, all subsequent reads silently empty). The Return path now routes the value through `coerce_array_to_vec` (which calls `gos_rt_vec_from_arr`) when the shapes match.

### `HashMap.iter()` direct-binding guard

- **MIR's method-call dispatch rejects `.iter()` on a `HashMap` receiver outside the for-loop shape** - `for (k, v) in m.iter()` is still handled by `try_lower_for_hashmap_iter` (a real entries walk on every tier). The direct-binding form `let xs = m.iter()` previously dispatched the `*mut GosMap` receiver through `gos_rt_arr_iter`, which reads the map handle's first 8 bytes as a `GosVec` length header and walks garbage - silent miscompile / segfault. The dispatch now `return None`s for HashMap receivers so the compiler emits a clear error pointing users at `m.keys()` / `m.values()` / the for-loop form instead of producing a broken binary.

### `Vec<Struct>` place-indexing + fixed-array promotion

Two coordinated fixes close a class of multi-slot-element corruption under `gos build --release`.

- **`bodies[i].field` over a `Vec<Struct>` routes through `gos_rt_vec_get_ptr`** - the place-expression Index arm previously appended a flat `Projection::Index`, which the LLVM lowerer strode off the `*mut GosVec` *header* rather than the data buffer. Element 0 happened to alias the header's first field, so reads/writes past index 0 hit garbage (the chess / nbody struct-array corruption). The Index arm now detects a Vec / Slice base with multi-slot elements (consulting the base local's MIR-resolved type so promoted bindings are seen), materialises the element address via `gos_rt_vec_get_ptr`, and binds it to a `&elem`-typed local; the appended `Field` projection auto-derefs that pointer so both reads and writes land inside the Vec's storage.
- **`let mut [T; N]` promotion to `Vec` is gated on actual growth** - a `mut` array-literal binding was unconditionally rewritten to a heap `Vec`, even for an explicitly-sized `[Body; 5]` that is only indexed, field-mutated, or passed to a `[T; N]`-typed parameter. The promotion desynchronised the element stride at call boundaries (`energy(&bodies)` declared `&[Body; 5]` strode the GosVec header as inline data → NaN). The MIR builder now pre-scans the function body for growth / reshape receivers (`push`, `pop`, `insert`, `remove`, `extend`, `truncate`, `clear`, `retain`, `append`, `resize`, `drain`, `split_off`, `sort`, `sort_by`) and promotes a `let mut [literal]` only when its binding is grown somewhere; otherwise it keeps the inline fixed-array layout that matches every use site. `let mut xs = [3, 1, 2]; xs.push(4); xs.sort()` still promotes; `let mut bodies: [Body; 5]` passed to a `[Body; 5]` parameter no longer does.

### `sort_by` comparator over aggregate elements

- **The closure-lift pass no longer pins aggregate-typed comparator params to i64** - `xs.sort_by(|a, b| a.1 < b.1)` on a `Vec<(String, i64)>` produced a no-op / wrong order. Inference left the closure params `a` / `b` as `Var` (the expected `FnTrait((T, T) -> i64)` signature wasn't propagated into the closure body), and `lift_closures` blanket-pinned every unresolved closure param to i64. The lifted comparator then computed `a.1`'s field offset off a junk integer rather than the element pointer the runtime sort (`gos_rt_vec_sort_by_aggr`) passes it. The lift pass now walks each closure body first and skips the i64 pin for any param used through a `TupleIndex` / `Field` projection or as a method-call receiver - those params hold aggregates passed by pointer. Scalar comparator params (`|n| n * 2`) keep the i64 pin they need. Works without the previously-required explicit `|a: (String, i64), b: (String, i64)|` annotation.

### `for e in &Vec<Enum>` slot-pointer dereference

- **`lower_for_vec` checks slot width before treating the element as inline** - the for-loop helper previously flagged any `TyKind::Adt` element as "inline aggregate" and bound the loop variable to the slot's address. For multi-slot user structs (`Projection { a: i64, b: i64 }` = 16 bytes inline) that's the right move; field projections walk off the slot address. For single-slot Adts - enums, sentinel-handle structs whose 8-byte slot *holds* a heap pointer - the loop body needs the pointer value, one `gos_load` away. The previous binding handed each iteration the slot address; `match e { … }` then read the first 8 bytes of the heap allocation as the pattern scrutinee, every variant arm failed to match, and `for e in &Vec<Expr>` silently produced no output. The check is now `slot_bytes > 8` rather than just "is Adt"; single-slot Adts route through the scalar `gos_load(ptr, 0)` path.

### `Vec<UserStruct>` inline element width

- **`type_slot_bytes` for user `Adt` sums registered field widths** - every user struct collapsed to 8 bytes regardless of field count, so `gos_rt_vec_new(elem_bytes)` for a `Vec<Projection>` whose `Projection { a: i64, b: i64 }` is two slots reserved 8-byte slots, and each push truncated to the first field. `for p in &xs { p.b }` then read garbage at the wrong offset (and any `String` field's `len()` segfaulted on the stray pointer). `type_slot_bytes` now consults `tcx.struct_field_tys(def)` and returns the slot-sum × 8 for user structs, leaving sentinel stdlib structs (DirInfo, Output, ResponseStream, Response - `u32::MAX - 5 ..= u32::MAX`) at the pointer-sized 8 bytes their runtime helpers require.

### Typed-storage fast paths tolerate generic-Array receivers

- **`Op::IntArrayGetI64` / `Op::FloatVecGetF64` fall back to `Value::Array`** - the compiler's `flat_int_locals` / `flat_float_locals` tracking can outlive the receiver's concrete `Value::IntArray` / `Value::FloatVec` payload when the call-args path doesn't typed-promote across a function boundary. The runtime fast paths now accept the generic `Value::Array(Vec<Value::Int>)` / `Vec<Value::Float>` shape (one discriminant match per index) instead of aborting with `receiver lost flat invariant`. Hot-path performance is unchanged on the typed path; the fallback rescues calls that previously panicked.

### Regression coverage

`tests/bug_regressions.rs` gains tests pinning the above behaviours through both VM and LLVM AOT tiers:

- `deref_assign_through_mut_i64_runs_under_llvm` - LCG `*s = *s * K + C` runs correctly instead of segfaulting.
- `mut_self_field_compound_assign_writes_back` - `self.n += 1` writes back through the pointer.
- `multi_dim_fixed_array_index_walks_inner_strides` - `arr[i][j][k]` over `[[[T; A]; B]; C]` lands on the correct element.
- `env_args_empty_iter_does_not_segfault` - `for a in env::args() { … }` is a clean no-trip when no user args supplied.
- `vec_pop_on_typed_storage_shrinks_by_one` - `[i64]` / `[f64]` slices shrink by exactly one after `xs.pop()`.
- `hashmap_keys_router_does_not_get_shadowed_by_json` - `m.keys()` returns all keys regardless of registration order with module-prefixed bare-name pushes.
- `return_array_literal_coerces_to_slice` - array-literal return to a `Vec`/`Slice`-typed function produces a real GosVec.
- `typed_int_array_get_falls_back_to_generic_array` - `arr[i]` inside `fn slide(arr: [i64; N])` works for repeated calls inside a loop.
- `logical_and_or_short_circuit_in_compiled_tier` - `&&` / `||` short-circuit RHS evaluation under `gos build --release`.
- `sort_by_on_tuple_vec_orders_by_comparator` - `xs.sort_by(|a, b| …)` on a `Vec<(String, i64)>` orders by the comparator without explicit closure-param type annotations.
- `vec_of_struct_index_field_reads_and_writes_through_data_buffer` - `bs[i].x` read and `bs[i].x = v` write on a `Vec<Struct>` land in the Vec's storage.
- `mut_fixed_struct_array_not_promoted_keeps_layout_across_calls` - `let mut bodies: [Body; N]` passed to a `&[Body; N]` parameter keeps its inline layout.
- `mut_scalar_array_with_push_still_promotes_to_vec` - a `mut` array literal that calls `push` / `sort` still promotes to a heap Vec.
- `vec_of_enum_for_loop_dereferences_slot_pointer` - `for e in &Vec<Enum>` reads the heap pointer out of the slot before passing the element to the body.
- `vec_of_multi_slot_struct_round_trips_all_fields` - `Vec<Projection>` where `Projection` has multiple scalar fields preserves every field across `push` / `for` iteration.

## 0.9.0 - Production hardening, tooling, observability, and SQL pluggability

### Language

- **`?` on `Option<T>` and `Result<T, E>`** - `try_propagation_kind` selects the propagation shape; `ast_is_option_shaped` is the AST-level fallback when typechecker hasn't pinned the return type. Error paths auto-route through `gos_rt_error_from` so `let x: A = fallible_b()?` works when `A: From<B>`.
- **User `impl Iterator for T` end-to-end** across all three tiers - HIR `lower_for` splits into `lower_for_user_iter` (Adt receivers; threads through a `__for_iter` let-binding and a `.next() -> Option<T>` call) and `lower_for_inline` (range / array / Vec fast paths). MIR for-loop fast-path bails to the generic shape for Adt receivers. Interp's `invoke_method` + `apply_closure_capture_self` write a mutated `&mut self` back to the receiver place; `&self` / `&mut self` are typed `Ref<SelfType>` in HIR.
- **`UnknownTraitBound` (GT0011)** - `register_fn_sig` validates declared trait names against `known_builtin_trait` (the eight built-in kinds) + the user's `declared_trait_names`. Typo'd bounds (`Itarator` for `Iterator`) now surface as a type error with a span.

### Tooling

- **`gos bench [PATH] [--parallel N]`** - discovers every `#[bench]`-annotated function under `PATH` (defaults to `src/`) and reports `ns/op` plus `allocs/op` per benchmark. Per-bench iteration counts auto-tune to a 50ms calibration window (capped at 2^20); allocation deltas read from `gossamer_runtime::gc::stats().bytes_allocated`. `std::testing::Bencher` ships as the future-facing argument type; zero-arg `#[bench]` fns keep working.
- check.sh extended to mirror more of the CI workflow with Github Actions.

### Runtime - production safety

- **Stack-overflow guard** - `stack_guard::install_stack_guard()` runs at scheduler start and per worker. Unix installs `sigaltstack(2)` + `SA_ONSTACK` SIGSEGV handler with async-signal-safe diagnostics; Windows uses `SetThreadStackGuarantee` + `SetUnhandledExceptionFilter`. Faults outside the guard window restore `SIG_DFL` and re-raise.
- **`safe_daemon::daemonize`** - Unix `fork` + `setsid` + second-`fork` detach so `gossamer-std` (`#![forbid(unsafe_code)]`) can run a daemon without losing that guarantee. `Unsupported` on non-Unix.
- **OOM no longer crosses the FFI boundary** - `gos_rt_gc_alloc` + `gos_rt_aggr_alloc_leak` `alloc_zeroed`-null paths `eprintln!` + `std::process::abort()` instead of `std::alloc::handle_alloc_error` (which panics; panic-across-FFI into compiled Gossamer is UB).
- **FFI transmute audit** - `c_abi/mime.rs::mime_str` no longer launders the borrow into `&'static str` via `mem::transmute`; returns an owned `String`.
- **`WorkerHandleGuard`** - RAII over `WorkerSlot::thread_handle`. On panic-unwind, swap-to-0 and call `preempt::release_thread_handle`. Closes a long-running Windows-service handle leak.
- **Typed function-pointer registry** - `c_abi::fn_registry` with `FnKind` enum (I64ArgsToI64, EnvI64ArgsToI64, HttpHandlerBare/Env, SortCmp/SortCmpAggr, UnaryI64ToI64, BinaryI64ToI64, PredI64, JitEntry, GoSpawnEntry, CtxCancelI64, Generic). `verify` runs at every `gos_rt_fn_tramp_N` / `gos_rt_go_spawn_call_N` site; registered-with-different-kind aborts. `parking_lot::RwLock<HashMap>` keeps the read path uncontended.
- **`GosMutex` owner tracking** - `owner: AtomicI64`; cross-goroutine unlock aborts with a diagnostic rather than corrupting lock state.
- **`parking_lot::Mutex` everywhere** - every internal `std::sync::Mutex` migrated. No poisoning, smaller footprint, faster uncontended path. `.lock().unwrap_or_else(PoisonError::into_inner)` collapses to `.lock()`.
- **`tests/audit_unsafe.rs`** - CI gate asserts every `unsafe { ... }` block in `gossamer-runtime/src/` (excluding the FFI surface in `c_abi/` + `ffi.rs`) carries a `// SAFETY:` comment within 8 lines above. Backfills `http2_server.rs` + `stack_guard.rs`.
- **`gossamer-runtime::replay`** - deterministic record + replay modes via `GOS_TRACE` / `GOS_REPLAY`. Length-prefixed binary records cover channel send/recv, goroutine spawn/yield, RNG seed draws.

### Runtime - performance

- **`gos_rt_str_concat`** - single-allocation path via `alloc_cstring_from_slices` (was three allocations per concat). `try_extend_last_cstring` removed.
- **`ChannelInner.closed`: `AtomicBool`** (was `Cell<bool>`) - `close()` uses `compare_exchange` so concurrent close-and-recv races converge deterministically.
- **Scheduler yield-rate tracking** - per-worker `last_yield_micros: AtomicU64` + `process_start()` / `now_micros_since_start()` helpers. `should_yield()` uses `Acquire` ordering.
- **Interp allocator-pressure shaves** - `apply_closure_capture_self` borrows the self-param name as `&str` from the closure (was a per-call `String::clone`); `builtin_map_inc_at` builds the map key via `SmolStr::from_str(&str)` directly (was `to_string()` then wrap). Every user `impl Iterator` `.next(&mut self)` benefits.

### Garbage collector

- **Overflow safety** - `Heap`'s two `u32::try_from` sites `eprintln!` + `abort` instead of `.expect()`. Weak-ref `generation` widened to `u64` with `checked_add`; closes the 2^32-churn use-after-free.
- **Pause-time histogram** - `GcStats::pause_histogram` (6-bucket: `<100us` / `<1ms` / `<10ms` / `<100ms` / `<1s` / `>=1s`) updated per `collect()` cycle.
- **Precise pointer-mask tracing** - `gos_rt_gc_alloc_traced(size, mask_ptr, mask_len)` registers an aggregate with an explicit `u32` pointer-offset mask. The marker walks only the recorded offsets; `null` mask opts into the conservative word-scan. Closes the false-retention hazard from `i64` payload words colliding with live addresses.
- **`gos_rt_gc_collect` thread-local `CollectBuffers`** - the snapshot `HashMap`, marked `HashSet`, and worklist `Vec` live in a `thread_local!` cell and are `.clear()`'d (capacity preserved) between cycles. Removes the per-cycle alloc/free churn on HashMap-heavy workloads.
- **`gos_rt_fs_list_dir` / `gos_rt_fs_walk_dir`** - per-entry blobs now allocate through `gos_rt_gc_alloc` so the collector can reclaim them. The prior path leaked one 56-byte payload per directory entry.

### Codegen - LLVM

- **`render_ir_to_string(bodies, tcx, allow_fallback)`** - runs the standard LLVM pipeline and returns `.ll` IR as `String`. Used by snapshot / smoke tests in downstream crates.
- **`gos build --release` strict-lowering on by default** - `set_strict_lowering(true)`; any MIR shape the LLVM backend cannot lower is a hard build failure.
- **`pipeline_tmp_dir`** suffixes the per-process directory with a per-call atomic counter so parallel `render_ir_to_string` / `compile_to_object` calls don't trample each other's `unit.ll` / `unit.o`.
- **`crates/gossamer-codegen-llvm/tests/lower_shapes.rs`** - 14 deterministic tests hand-roll a `Body` per MIR shape (constants + binop variants for add/sub/mul/div/rem/and/or/xor/shl/shr) and assert substring properties on the rendered IR.

### Codegen - Cranelift

- **Closure-callback JIT dispatch entries** - `gos_rt_arr_sort_by_i64`, `gos_rt_vec_sort_by_i64`, `gos_rt_vec_sort_i64`, `gos_rt_{arr,vec}_sort_by_aggr`, `gos_rt_callback_invoke`, `gos_rt_iter_map_i64` now in the JIT symbol table. User bodies calling these no longer skip JIT compilation.
- **`intrinsic_g{0,1,2,3}.rs` → `intrinsic_{io_math,collections,handles,string}.rs`** - names describe contents rather than alphabet position; module-level docs added.

### Diagnostics + LSP + Parse + CLI

- **Centralised error-code registry** - `gossamer-diagnostics::REGISTRY` is the single source of truth for every `GL`/`GP`/`GR`/`GT`/`GM`/`GX` code; `gos explain CODE` reads from it. `tests/registry.rs` enforces alphabetical order + non-empty text; `tests/snapshots.rs` renders every code (plain + framed) via `insta`.
- **LSP - 67 new integration tests** across `completion`, `hover`, `diagnostics`, `document_symbol`, `code_actions`, `format`, `semantic_tokens`, `inlay_hints`. `ServerHandle` (test-only) gains 13 request methods + four `params`-building helpers.
- **`crates/gossamer-parse/tests/proptest_round_trip.rs`** - five proptest properties exercise int literals, binary ops, `let` bindings, function definitions, and nested blocks. Capped at `cases: 64` / `max_shrink_time: 2s` for CI determinism.
- **`crates/gossamer-cli/tests/repl.rs`** - seven scripted-stdin tests for the `gos repl` binary covering the happy path and error reporting.
- **`examples/projects/rust_binding_add/`** - minimal Rust-bindings project demonstrating `gos add --rust-binding`.

### Stdlib - `std::database::sql`

- **`pool` submodule** - bounded-semaphore connection pool with idle-timeout recycling and a per-checkout retry budget.
- **`migrate` submodule** - forward-only schema migrations from a `<version>_<slug>.sql` directory; each migration runs in its own transaction; concurrent runners coordinate via an advisory lock on `schema_migrations`.
- **`query::Select` builder** - fluent SELECT renderer emitting `(sql, params)` with `Value`-bound parameters and Postgres-style `$N` placeholders (SQLite also accepts).
- **Trait surface extensions** - `Error::driver(...)` + `Error::PoolExhausted`, `IsolationLevel` enum (`Default` / `Read{Uncommitted,Committed}` / `RepeatableRead` / `Serializable`). `Conn::begin_with(iso)` / `ping()` / `execute_many(sql, rows)` ship as default impls on the facade for incremental driver adoption.
- **Native lowering** - `gossamer-runtime::sql` (trait surface relocated from `gossamer-std`) + `c_abi::sql` (33 `gos_rt_sql_*` shims over five handle registries: Conn / Stmt / Rows / Row / Tx / Value). Cranelift JIT + LLVM AOT dispatch through `Both`-tier ABI registry entries. `Conn::interrupt()` / `execute_ctx(ctx, ...)` / `query_ctx(ctx, ...)` check `ctx.is_cancelled()` on either side of the call.
- **SQLite driver removed** - `rusqlite` dependency dropped, `database/sql/sqlite.rs` deleted. The facade stays; third-party drivers register through `gossamer-runtime::sql::Driver`.

### Stdlib - web + networking

- **`std::http_h3`** - first-party HTTP/3 server + client (RFC 9114) wrapping `quinn` (QUIC) + `h3`. Each `serve` / `Client` instance owns a private current-thread tokio runtime; callers see only synchronous entry points mirroring `std::http_h2` and `std::http`.
- **`std::http_native_client` TLS** - `NativeClient` wraps the TCP stream in `rustls::StreamOwned<ClientConnection, _>` for `https://`; per-request setup amortises through `Arc<rustls::ClientConfig>`.
- **`http_state::attach_to_router`** - `Router` gains an optional `AppState` field + `set_state` / `state` accessors; `State::<T>::from_router(&router)` is the typed extractor handlers use.

### Stdlib - observability + compression

- **`std::metrics`** - Prometheus-compatible `Counter`, `Gauge`, `Histogram` + a `Registry` holding them in registration order; outputs the text-exposition format.
- **`std::trace`** - W3C trace-context distributed tracing (`TraceId`, `SpanId`, `SpanContext`, `Span`, `Tracer`). OTLP JSON exporter pushes ended spans to a sidecar collector - no `opentelemetry-otlp` dependency.
- **`std::compress::zstd`** - Zstandard encoder/decoder wrapping vendored libzstd. Same byte-in/byte-out shape as `gzip` / `flate` / `zlib`; level 1-22, default 3.

### Stdlib - fs

- **Watch / mmap / locks / atomic writes** - `fs::watch::Watcher` (`notify`), `mmap_read` / `mmap_write` (`memmap2`), `lock_exclusive` / `lock_shared` (`fs2`), `write_atomic` (temp-file + rename). `hard_link`, `set_permissions_mode`, `chown` close the niche-fs gap.
- **`fs::TempDir`** - RAII temp directory; `new()` / `with_prefix(prefix)` under `env::temp_dir()`; `path()` / `into_path()` / `Drop`-cleanup.
- **`fs::temp_file(prefix)`** - `(File, PathBuf)` for a uniquely-named writable scratch file.

### Stdlib - crypto + jwt

- **Hex digest C-ABI shims** - `gos_rt_sha256_hex` / `gos_rt_sha512_hex` / `gos_rt_blake3_hex` / `gos_rt_hmac_sha256_hex` under `c_abi::crypto`, alphabetically registered in `gossamer-abi::registry`. Tier-parity bit-identical via `feature-testing-examples/crypto_sha_hex.gos`.
- **`std::jwt` RS256 / RS384 / RS512 verify** - RSA PKCS#1 v1.5 via `ring`'s audited constant-time RSA. The vulnerable `rsa` crate (RUSTSEC-2023-0071) stays out of the tree.

### Stdlib - unicode + iter + regex

- **Grapheme cluster iteration** - `std::unicode::graphemes(s)` / `grapheme_count(s)` walk UAX #29 extended grapheme clusters via `unicode-segmentation`. `👨‍👩‍👧` counts as one.
- **`std::iter::Lazy<I>`** - lazy adapter over any Rust `Iterator` with `map` / `filter` / `take` / `skip` / `step_by` adapters and `sum` / `min` / `max` / `count` / `first` / `fold` / `any` / `all` / `to_vec` / `product` terminals. Allocation-free until the terminal materialises.
- **Free iter combinators** - `iter::sum`, `iter::product`, `iter::min`, `iter::max`, `iter::step_by`, `iter::once`, `iter::empty`, `iter::collect` join the existing family.
- **Regex named groups** - `regex::capture_names(pat)`, `regex::captures_named(pat, hay)`, `regex::captures_named_all(pat, hay)`. `(?P<year>\d{4})` lookups return `HashMap<String, String>` directly.

### CI

- **`cargo doc --workspace`** under `RUSTDOCFLAGS=-D rustdoc::broken_intra_doc_links` + `cargo test --doc --workspace --release` - doc-test drift fails CI.
- **Cross-target check matrix** - `aarch64-unknown-linux-gnu`, `riscv64gc-unknown-linux-gnu`, `wasm32-unknown-unknown`, `wasm32-wasip1` each `cargo check` the platform-agnostic crates (runtime, abi, binding{,-macros}, pkg, gc, sched).

### Test fixtures

- **`feature-testing-examples/iterator_trait_user_impl.gos`** - user `impl Iterator for Counter` driving `for x in c`; tier-parity bit-identical.
- **`feature-testing-examples/try_option_propagation.gos`** + **`try_err_conversion.gos`** - `?` on `Option` and `?` with `From`-conversion in the error path.
- **`feature-testing-examples/crypto_sha_hex.gos`** - every hex-digest shim exercised end-to-end.
- **`crates/gossamer-runtime/tests/gc_collect_concurrent.rs`** - concurrent-allocator non-starvation, precise pointer-mask registration, fs blob GC reclaimability.
- **`crates/gossamer-std/tests/{iter_lazy,python_ergonomics}.rs`** - `iter::Lazy` chain round-trips; regex named groups + `TempDir` cleanup + `temp_file` uniqueness.
- **`crates/gossamer-hir/tests/lower.rs`** - trait-bound validation, Option-shape `?` propagation.

## 0.8.0 - Unicode, web stack, publish flow, LSP, fixes, and Rust-binding ergonomics

### Language

- Identifiers follow UAX #31 `XID_Start` / `XID_Continue` (same surface as Rust 2024) - `let café = 1`, `let π = 3.14`, `let 名前 = "x"` all parse.
- New `docs_src/language/` reference site (33 pages: `if_let`, `while_let`, `pipe`, patterns, traits, …) generated from the manifest registry.

### Stdlib - std::unicode

The hand-rolled ASCII / BMP-range stubs are gone; every predicate answers against the Unicode 16 tables via `unicode-properties`, `unicode-normalization`, and `unicode-segmentation`.

- General-category predicates now correct for non-ASCII: `is_digit('٧')` (Arabic-Indic), `is_punct('-')` (em dash), `is_symbol('¥')`, `is_mark('\u{0301}')`, `is_number('Ⅴ')`, `is_title('ǅ')`.
- Added `is_assigned(r) -> bool` and `combining_class(r) -> i64`.
- Added whole-string casing helpers: `to_upper_str(s)` (ß → SS), `to_lower_str(s)` (Σ → σ), `fold_case(s)`.
- Added normalization: `nfc(s)`, `nfd(s)`, `nfkc(s)`, `nfkd(s)`, plus `is_nfc` / `is_nfd` / `is_nfkc` / `is_nfkd`.
- Added segmentation (UAX #29): `graphemes(s) -> Vec<String>`, `grapheme_count(s) -> i64`, `words(s)`, `word_bounds(s)`, `word_count(s)`, `sentences(s)`, `sentence_count(s)`. Family ZWJ sequences count as one grapheme; `cafe\u{0301}` is four.

### Stdlib - std::utf8

- `full_rune_in_string(s) -> bool` exposed alongside the existing byte-slice `full_rune`.

### Stdlib - HTTP server stack

- `std::http::cookie` - RFC 6265 `Cookie` / `CookieBuilder`, `SameSite` enum, `parse_cookie_header`, `parse_set_cookie`.
- `std::http::csrf` - double-submit cookie + Origin/Referer check: `issue_token`, `verify_token`, `extract_token`, `origin_allowed`, `check`, `attach_cookie`, `RouteAuth`.
- `std::http::form` - `application/x-www-form-urlencoded` parser and builder.
- `std::http::multipart` - streaming RFC 7578 parser: `parse_boundary`, `parse_bytes`, `parse<R: Read>`, with `Part` / `PartData` / `Form` types.
- `std::http::query` - typed `Query` wrapper over URL query strings.
- `std::http::session` - signed-cookie session store: `SessionConfig`, `Session`, `SessionStore` trait, `SignedCookieStore`, `with_session`.
- `std::http::state` - `AppState` typemap + `State<T>(Arc<T>)` DI container for handlers.
- `std::http::health` - `Probe` trait + `Health` aggregator with `always_ok` / `always_fail` / `tcp_probe` helpers.

### Stdlib - HTTP middleware

`std::http::middleware` gained `body_limit`, `timeout`, `hsts`, `security_headers`, `cache_control`, `etag`, `bearer_auth`, `rate_limit`, `compress_gzip`, and a `safe_defaults` bundle - alongside the existing `logger`, `recoverer`, `request_id`, `cors`, and `basic_auth`.

### Stdlib - HTTP/2

- Server push: `PushOptions`, `PushStream`, `ResponseWriter::push_promise`.
- Trailers: `ResponseWriter::write_trailers`, `Request::trailers`, `Trailers` alias.

### Stdlib - std::process / std::exec

- `Pipeline` for stdout→stdin chaining: `pipeline_run`.
- `Signal` enum + `spawn`, `kill`, `signal(pid, sig)`, `kill_group(pgid, sig)`, `wait_timeout(child, ms)`.

### Stdlib - new modules

- `std::jwt` - RFC 7519 sign + verify for HS256/384/512, ES256, and EdDSA; `Alg`, `Header`, `Claims`, `VerifyOpts`.
- `std::lifecycle` - graceful-shutdown hooks, signal handling, sd_notify.
- `std::validate` - `Validate` trait plus `FieldError` / `Errors` for form/field validation.
- `std::crypto::password` - Argon2id facade: `hash`, `verify`, `needs_rehash` (PHC strings).

### Package manager

- `gos publish` / `yank` / `login` / `logout` / `owner` - full registry workflow.
- Credential store (`~/.config/gossamer/credentials.toml`): `CredentialStore`, `Credential`, `load_default`, `get`, `insert`, `remove`.
- Ed25519 publish keys: `load_publish_key`, `sign_bytes`, `verify_bytes`.
- `pack_crate`, `upload_with`, `yank_with`, `owner_op_with` round out the publish pipeline.
- Transitive resolution: `CatalogueEntry`, `resolve_transitive`, `CacheBackedLoader`, `FnLoader`, `NoopLoader`.
- Disk-backed source cache under `default_cache_root()` (digest-keyed).
- Tarball + Git + registry sources with sha256 verification; `tarball_sha256` recorded in `LockedEntry`.
- `[rust-bindings.<name>] src = "bindings/x.rs"` - single-file binding; `gos` auto-scaffolds a wrapper crate under `.gos-bindings/__srcwrap-<name>/` with an optional `deps = "..."` Cargo-deps fragment.
- `[rust-bindings.<name>] prebuilt = "lib/x.a", abi = "1.0"` - pre-built static archive for hermetic / no-cargo-at-build-time deployments.

### LSP

- New request handlers: `textDocument/typeDefinition`, `references`, `documentHighlight`, `prepareRename`, `rename`, `inlayHint`, `documentSymbol`, `workspace/symbol`, `foldingRange`, `signatureHelp`, `formatting`, `codeAction`, `semanticTokens/full`.
- Cross-file `WorkspaceIndex` (`SymbolBucket` over Items / Variants / Fields / Methods, qualified `SymbolKey`) rebuilt incrementally on `didOpen` / `didChange`; powers references + rename across files.

### Rust bindings

- `gossamer-binding` ABI frozen at (1, 0). Wire shapes (`GosVec`, `GosVariant`, `GosVariantValue`, `GosTuple`, `GosBytes`, `BindingGosMap`, `GosDynVariant`, `GosCallback`) are stable; minor releases add shapes but never reorder fields.
- New `#[gos_module("path")]` proc-macro attribute: replaces `register_module!`'s triple-declaration; auto-publishes `__bindings_force_link()` via `FORCE_LINK_FNS`; doc-comments flow through to `gos doc`.
- `register_module!` gains a `name: <ident>` short form with compile-time `SigType` validation per param + return.
- New `#[derive(GosStruct)]` for user structs (round-trips through `Value::Struct` / `GosDynVariant`).
- New `#[gos_opaque]` on `impl Type` blocks: each `pub fn` becomes a binding item named `Type::method`.
- New `#[gos_blocking]` attribute: dispatches the body through a blocking pool with inline fallback.
- Extended type vocabulary: `Option<T>`, `Result<T, String>`, `Result<T, GosError>` for common `T`; `HashMap<String, Vec<i64>>`, `<i64, String>`, `<String, bool>`, `<String, f64>`; tuples up to arity 4 with generic `SigType` / `FromGos` / `ToGos`.
- New `GosError` with `From` for `io::Error`, `ParseIntError`, `ParseFloatError`, `Utf8Error`, `FromUtf8Error`, `fmt::Error`, `SystemTimeError`, `Infallible`; propagates via `?` with full cause chain on render.
- New `PersistentCallback`: long-lived callable handle that survives past the binding return (complements the call-scoped `BindingCallback`).
- New `gossamer-binding-macros` proc-macro crate; re-exported transparently from `gossamer-binding`.

### CLI

- `gos test --coverage <path>` writes lcov reports; `--parallel N` / `--serial`, `--format junit`, `--tier-parity --report=status`.
- `gos feature-status` - list and check the feature-status registry: `--status shipped|experimental|planned|removed`, `--format table|json|markdown`, `--check` drift gate.
- New `std::manifest::feature_status` registry covers stdlib modules and `lang::*` entries; rendered docs gain a `Status:` marker per page.
- `gos new --template binding NAME` scaffolds a ready-to-edit binding crate.
- `gos bindgen INPUT --output DIR --module PATH` walks a Rust source file's `pub fn` surface, classifies each by ABI vocabulary support, and emits a ready-to-edit binding crate; unsupported items are flagged with their blocking type.

### Runtime

- Coverage: `runtime::coverage::{Counter, register, bump, record, snapshot, reset, set_enabled}` plus C-ABI shims `gos_rt_cov_record`, `_bump`, `_reset`, `_set_enabled`.
- Exec C-ABI shims: `gos_rt_exec_pipeline_run`, `_signal`, `_kill_group`, `_wait_timeout`.
- Unicode C-ABI: 37 `gos_rt_unicode_*` shims (predicates, case, normalization, segmentation).
- UTF-8 C-ABI: 9 `gos_rt_utf8_*` shims (rune count, validity, boundaries).
- Vec/array slice helpers: `gos_rt_intarr_slice_result`, `gos_rt_floatarr_slice_result`, plus the existing string and generic Vec variants.
- Panic traces: per-goroutine call-stack tracker (`Frame`, `set_active_gid`, `stack_push` / `_pop` / `set_active_line`, `active_frames`, `render_active_panic_trace`); both backends emit prologue/return push+pop calls.

### Compiled tier

- Every new `std::unicode`, `std::utf8`, `std::exec`, and slice helper has a typed entry in the ABI registry and a dispatch arm in MIR `stdlib_free.rs` / `method_call_dispatch.rs`. Bit-identical output across VM, Cranelift, and LLVM tiers - verified by `feature-testing-examples/unicode_full.gos`, `slice_methods.gos`, `exec_pipeline.gos`, `exec_signal_group.gos`, `exec_wait_timeout.gos`, `http2_push.gos`, and `http2_trailers.gos` under `tier_parity`.
- Cranelift soft-zero fallback for unknown call names removed - unresolved calls are now a hard error (the `GOSSAMER_STRICT_LOWER` env var is retired).

### Fixes

- LLVM tier silent miscompile when `if let Some(p) = m.get(&k); p.field` was used for `HashMap<_, Struct>`: the dispatcher pinned the call's return type to bare `i64`, so the match arm couldn't recover `&V` from `Option<V>` and field projection fell through to `ptr`. New `gos_rt_map_get_i64_opt` / `gos_rt_map_get_str_opt` return `Option<V>` as a `*mut GosResult`, with `pinned_ret` synthesised from the receiver's HashMap value Ty. Side effect: `m.get(missing)` for `HashMap<_, i64>` now correctly returns `None` (previously the no-Adt happy-path encoded missing keys as `Some(0)`).
- LLVM tier stack-pointer bug on `HashMap.insert` with struct values: the inserted value was the stack address of the literal alloca, so subsequent reads in any other frame saw stale data. `maybe_heap_copy_aggregate` heap-copies the struct via `gos_rt_aggr_alloc` before passing to `gos_rt_map_insert_i64_i64` and `_str_i64`. The wrapper is marked `#[inline(never)]` plus a `#[used]` static anchor (`GOS_RT_AGGR_ALLOC_KEEP`) so neither the rustc inliner nor the linker's dead-strip collapses it back into `gos_rt_gc_alloc` - that collapse silently elides the heap copy and reintroduces the stack-pointer regression. Cross-tier parity verified by `feature-testing-examples/hashmap_get_some_field.gos` and the aether_ecs build benchmark, which now matches the interp tier bit-for-bit (`pos_x_sum=9990959.95`).
- LLVM tier GC blindness through `HashMap` values: `GosMap` allocations live outside the GC registry (`Box::into_raw` in `gos_rt_map_new`), and the conservative payload scan can't walk the Rust-side bucket allocator, so heap-allocated struct values stored as i64 entries were unreachable from the tracing collector and reclaimed mid-program. `gos_map_register` / `gos_map_deregister` track every live `GosMap` in a dedicated registry; `gos_rt_gc_collect` now adds a second mark drain that walks every registered map's storage and emits each value as a candidate pointer for the registry-presence check. The conservative trace tolerates raw i64 values in primitive maps (`HashMap<_, i64>`) - they don't match registered allocations so they don't over-mark.

### Tooling

- `check.sh` runs `gos feature-status --status experimental --check` to gate accidental drift.
- New CLI test surface: `feature_status.rs`, `http_h2_alpn.rs`, `http_h2_conformance.rs`; LSP `workspace_refs_rename.rs`; pkg `registry_publish.rs`.
- Fuzz harnesses (smoke + weekly) now cap inputs with `-rss_limit_mb=2048 -malloc_limit_mb=2048 -timeout=30` so a single pathological seed records a crash artefact instead of OOM-killing the runner.
- `gossamer-runtime::ffi::tests::opens_libc_and_calls_strlen` and the `gossamer-coro` test suite gated behind `#[cfg(not(miri))]` (`libloading::dlopen` and `corosensei`'s `mmap(PROT_NONE)` are unsupported by Miri); host-CPU runs still cover both.

### Cross-platform

- `std::signal` on Windows now bridges `SetConsoleCtrlHandler`: Ctrl+C → SIGINT, Ctrl+Break → SIGQUIT (+ goroutine-stack dump via `sigquit::render_to`), close / logoff / shutdown → SIGTERM. Previously `signal::on(SIGINT).wait()` deadlocked because nothing flipped the notifier flag.
- `std::lifecycle` Windows arm consumes those notifiers - `Lifecycle::install_default()` now runs registered shutdown hooks on Ctrl+C / supervisor close, mirroring the unix dispatcher's double-signal force-exit semantics.
- `find_clang_rt_profile` searches macOS Homebrew (`/opt/homebrew/opt/llvm@*`, `/usr/local/opt/llvm@*`, `darwin/libclang_rt.profile_osx.a`) and Windows MSYS2 (`C:\msys64\mingw64\lib\clang\*\lib\windows\clang_rt.profile-*.lib`) layouts; honours `$GOS_LLVM_PROFILE_RT` for explicit overrides and walks the `$GOS_LLVM_OPT` parent tree.
- `std::net::UnixListener` / `UnixStream` gain `#[cfg(not(unix))]` stub arms so `use std::net::UnixListener` resolves on Windows; every method returns `Err("AF_UNIX sockets are not supported on this platform")` until the real Win10+ AF_UNIX surface lands.
- `gossamer-std`'s `unicode-properties` / `unicode-normalization` / `unicode-segmentation` deps moved out of `[target.'cfg(unix)'.dependencies]` - they were used unconditionally by `std::unicode` and would have failed to resolve on a Windows build.

## 0.7.0 - Stdlib, stability, refactoring, and build optimizations

### Build

- Debug builds use a minimal opt pass set (`mem2reg`, `instcombine`, `simplifycfg`) instead of `-O1`; cuts `gos build` wall-clock time by 100-200 ms on typical programs.
- Release builds parallelize per-body `opt`+`llc` across up to 8 threads; wall-clock time falls roughly `(N-1)/N` on N-body programs.
- Incremental object cache under `~/.cache/gossamer/ir-cache` (or `GOS_BUILD_CACHE`); repeat builds reuse unchanged bodies. Disable with `GOS_NO_CACHE=1`.

### Performance

- `gos_rt_panic_oob`, `gos_rt_panic`, and `gos_rt_process_abort` declared `noreturn cold nounwind` in emitted LLVM IR; restores inner-loop vectorization that the 0.6.0 bounds-check pass had blocked.

### Stdlib - new modules

- `std::encoding::yaml::to_json` / `from_json` - YAML ↔ JSON text converters, mirroring `toml::to_json` / `from_json`.
- `std::sync::Map` - concurrent string-keyed string-value map: `new`, `set`, `get`, `delete`, `len`, `contains`, `keys`.

### Stdlib - string

All methods also available as `std::strings` free functions.

- `s.split_once(sep)` / `s.rsplit_once(sep)` → `Option<(String, String)>`
- `s.count(needle) -> i64` - non-overlapping occurrence count
- `s.strip_chars(cutset)` / `s.lstrip_chars(cutset)` / `s.rstrip_chars(cutset)`
- `s.zfill(width)` and `s.center(width, pad_char)`
- `s.slice(start, end) -> Result<String, errors::Error>` - non-panicking byte-range slice

### Stdlib - Vec / `[T]`

- `xs.contains(&v) -> bool`, `xs.index_of(&v) -> Option<i64>`, `xs.count_of(&v) -> i64`
- `xs.first() -> Option<T>` and `xs.last() -> Option<T>`
- `xs.reversed() -> Vec<T>` - non-mutating counterpart to the in-place `xs.reverse()`
- `xs.slice(start, end) -> Result<Vec<T>, errors::Error>`
- `Vec::insert(xs, i, v) -> Result<Vec<T>, errors::Error>` and `Vec::remove(xs, i) -> Result<T, errors::Error>` - safe qualified forms; the legacy method-call shape keeps its existing behaviour

### Stdlib - HashMap

- `m.keys() -> Vec<K>` and `m.values() -> Vec<V>`
- `HashMap::pop(m, k) -> Option<V>` - removes and returns the previous value

### Stdlib - scalar prelude

- `min(a, b)`, `max(a, b)`, `clamp(x, lo, hi)` - bare prelude functions for scalar pairs

### Stdlib - auto-derive

- Narrow integer fields (`i8`, `i16`, `i32`, `u8`, `u16`, `u32`, `f32`) now supported in `from_json` / `to_json` auto-derive; previously the entire struct was silently skipped.
- `from_yaml` / `to_yaml` auto-derived on every eligible struct alongside the existing JSON and TOML pairs.

### Stdlib - misc

- `flag::Cell<T>` auto-derefs at comparisons, function arguments, and typed register unboxes; `*flags.field` still works explicitly.
- `errors::newf(fmt, args…)` - format-shaped error constructor; rewritten at parse time to `errors::new(format!(fmt, args…))`.
- `http::Response.raw_bytes` - body as `Vec<u8>` for binary responses; compiled tier now matches the VM tier.
- `os::write_file(path, &Vec<u8>)` - binary-safe write preserving embedded NULs.
- `os::read_file(path) -> Result<Vec<u8>, errors::Error>` - raw bytes counterpart to `read_file_to_string`.

### Fixes

- LLVM `slot_count` for `http::Response` corrected to `None`; the previous inline-alloca layout truncated the heap pointer, causing segfaults in LLVM AOT builds when accessing `.body`.
- Resolver allows user-defined items to shadow prelude entries without collision.

## 0.6.0 - Stability hardening

### Safety

- `catch_unwind` at every `gos_rt_*` and JIT-call boundary - runtime
  panics no longer cross `extern "C"` as UB.
- Recoverable language panics (e.g. chan double-close) return a typed
  error instead of `process::abort`.
- `gos_rt_str_free` validates the allocator tag before freeing.
- No `process::abort` / `process::exit` outside sanctioned entries.

### Codegen

- Cranelift sign discipline: `coerce_arg_to` / `coerce_store_value`
  sign-extend by default; `Shr` dispatches `sshr` vs `ushr` from MIR
  operand type.
- Bounds checks on dynamic array indexing in both backends; opt out
  via `GOSSAMER_DISABLE_BOUNDS_CHECK`.
- Cranelift soft-zero fallback for unknown call names warns at
  compile time; `GOSSAMER_STRICT_LOWER=1` promotes to a hard error.
- LLVM IR verification (`opt -passes=verify`) runs before the
  optimisation pipeline.
- LLVM `gos_rt_*` declarations route through a single `declare_rt`;
  the synthesized-decl path is gone.
- Cranelift `Rvalue::Aggregate` allocates through `gos_rt_aggr_alloc`
  (GC-tracked) rather than raw `calloc`.

### Containers

- Typed `Vec<T>` allocation in both backends. `Vec<String>`,
  `Vec<Vec<_>>`, `Vec<HashMap<_,_>>` emit `gos_rt_vec_new_typed`
  with an element-kind tag.
- `gos_rt_vec_free` deep-frees STRING / VEC / MAP / ERROR element
  payloads via the elem-kind tag.
- `gos_rt_vec_push` clones inbound strings for STRING-typed vecs
  into the tagged allocator domain.

### IR validation

- MIR verifier gained 8 type-aware checks (call arity vs callee,
  return ty != Error, aggregate operand count, branch cond is bool,
  drop target is owning, unary-neg `i128::MIN`, switchint disc
  int/bool, call dest typed). Runs in `debug_assertions` at every
  pass boundary.
- Bytecode validator runs at `Vm::load` (PC bounds, register
  bounds, jump targets, constant-pool bounds).
- Conditional-init drop pass is now flow-sensitive (forward must-init
  dataflow); refuses uninit free path-sensitively.
- `i128::MIN` const-fold uses `checked_neg` (was overflow-panic).

### Frontend

- Recursion-depth cap (256) on parser, type-checker, and HIR lowerer
  with `GP0017` / `GT0008` diagnostics. Closes brace-bomb crashes.
- Parse-error nodes are typed: `ExprKind::Error` / `PatternKind::Error`
  replace silent `Literal::Unit` / `Wildcard` fallbacks.
- Integer-literal magnitude validation at typecheck (`GT0009`).
- `\u{...}` / `\x..` string escapes decoded with surrogate /
  ASCII-bound validation.

### Binding ABI

- `ABI_VERSION = (0, 6)` const plus `__gos_binding_abi_version`
  static the runtime sniffs at startup.
- Runtime `GosMap` and binding-side `BindingGosMap` layouts split;
  new `gos_rt_binding_map_free` for the binding struct.
- `gos_rt_callback_invoke` is a loud stub (eprintln + zero-fill of
  `result_out`); closes the silent-Err(-1) regression.

### Runtime

- `gos_rt_http_serve` bounded thread spawn: `GOSSAMER_HTTP_MAX_CONN`
  (default 4096); past the cap responds 503.
- VM `MAX_CALL_DEPTH = 512` in release (was 40).

### Tracing GC connected end-to-end

The compiled tier now has an active tracing collector.

- Raw-pointer aggregate registry (`gos_rt_gc_alloc` /
  `gos_rt_aggr_alloc`) backed by a `HashMap<usize, AllocEntry>`
  carrying `(size, mark, generation)`. Tracking is on by default;
  `GOS_GC=leak` opts out for benchmarks measuring raw-allocator cost.
- Stop-the-world conservative mark + sweep (`gos_rt_gc_collect`).
  Mark phase snapshots every thread's raw-pointer shadow stack and
  transitively traces each marked allocation's payload with
  pointer-sized validated word scans (alignment, bounds, and
  registry-presence checked per word). Sweep deallocates unmarked
  entries and bumps the registry generation so cross-thread races
  against a stale snapshot fail fast.
- Thread-local raw-pointer shadow stack with `gos_rt_gc_root_push`,
  `gos_rt_gc_root_save`, `gos_rt_gc_root_restore`. Stored as
  `usize` so `Send + Sync` is structural, not bespoke.
- Safepoints at function prologues and loop back-edges in both
  Cranelift and LLVM. A per-function MIR pre-scan identifies
  back-edge targets; codegen opens those blocks with
  `gos_rt_gc_safepoint()`. Atomic-load + compare in the common case;
  runs a full collect when `GOS_GC_THRESHOLD` (default 4 MiB) trips.
- Per-function root save/restore emitted at every prologue and every
  return in both backends. Aggregate-return heap copies push the
  returned pointer after the callee's restore so the root persists
  into the caller's frame.
- `Layout::from_size_align_unchecked` removed from the GC path; every
  allocation routes through a single validated helper that fails fast
  on overflow or bad alignment.
- Cycle reclamation proven by a runtime unit test plus two
  cross-tier stress regressions (10 000-iteration aggregate loops
  under `GOS_GC_THRESHOLD=4096` across VM, debug LLVM, and release
  LLVM). Spectral-norm at `N=5500` produces the bit-exact reference
  value `1.274224153` with the collector firing throughout.

### Tracing GC hardening

- `PtrKey` reduced to a `usize` newtype. Registry is structurally
  thread-safe; pointer dereference happens only after registry
  validation under the collect lock.
- `ThreadRoots.stack` stores `usize`; the marker is the sole code
  path that converts back to a pointer, only after re-validating
  the address against the registry.
- Generation counter on `AllocEntry` bumped at sweep so a pointer
  the marker observed in a stale shadow-stack snapshot can't be
  silently re-traced. Marker checks `(addr, generation)` together.
- Word scan replaces raw pointer arithmetic with `scan_payload_words`,
  which re-derives word count from the registry's authoritative size
  and uses `ptr::read_unaligned` defensively.
- Shadow-stack bounded growth: per-thread stack capped at
  `GOS_GC_SHADOW_MAX` (default 1 048 576); pushes past the cap
  trigger an immediate stop-the-world collect.
- `gos_rt_write_barrier_ptr(slot, new_val)` exposed as a runtime
  symbol (no-op under STW); reserves the ABI slot for a future
  concurrent-mark phase.
- `gos_rt_gc_assert_consistent()` debug-only registry walker wired
  into the STW collect path.
- Miri-clean GC unit tests (`cargo +nightly miri test -p
  gossamer-runtime --lib tracing_gc_tests`).
- Every `unsafe` block in the GC path carries a structured SAFETY
  comment (provenance, aliasing, synchronization, failure mode).

### Stdlib

- Auto-derived `<Type>::from_json(text)` / `<Type>::to_json(self)`
  on every user struct. Strict, one-line, serde-style
  (de)serialization built at `Vm::load` from the typechecker's
  field-type table. The decoder validates each field against its
  declared shape and rejects type mismatches and missing required
  fields with a path-qualified error (e.g.
  `User::from_json: field 'age': expected integer, got string`).
  Nested structs resolve by source name; `[T]` / `Vec<T>` /
  `[T; N]` / tuples / `Option<T>` / `HashMap<String, V>` walk
  recursively; `json::Value` fields pass through untouched.

### Cleanup

- Deleted dead interpreter modules (`peephole.rs`, `goroutine_pool.rs`).

### Tooling

- Toolchain locked to Rust 1.95.0 across the repo: `channel =
  "1.95.0"` and `profile = "minimal"` in `rust-toolchain.toml`,
  workspace MSRV bumped to 1.95, every CI `dtolnay/rust-toolchain`
  reference pinned to `@1.95.0`, the `rustup default stable` step
  in the shim-guard replaced with `rustup show` (idempotent,
  serial), and a `rustup set profile minimal` step inserted after
  every dtolnay install (including the nightly fuzz / miri /
  sanitizer jobs). The redundant MSRV CI job is gone.
  Without all three locks in place, the GitHub Actions runner
  images' user-default rustup profile is `complete`, so the
  rustup-shim invoked by cargo decides the project needs rust-src
  and races the parent + every nested `build.rs` cargo invocation
  to download it - one of them dies with `could not rename
  'downloaded' .partial` (Linux) or `detected conflict:
  rust-src Cargo.lock` (macOS / Windows, where the runner image
  has a partial rust-src dir from a previous stable build). The
  three locks make rustup stop deciding rust-src "should be
  there".
- Weekly fuzz + corpus-minimization jobs moved out of `fuzz.yml`
  into a separate `fuzz-weekly.yml`. The `if: github.event_name
  == 'schedule'` gate hid them on push / PR, but the GitHub UI
  still rendered each skipped job with its unexpanded
  `${{ matrix.target }}` placeholder. A schedule-only file is
  cleaner.
- `check.sh` fuzz loop covers all 10 targets (added `resolve`,
  `hir_lower`, `vm_run` - they were missing, which is how the
  CI build broke without local notice).
- CI runners standardised on `*-latest` (`macos-13` pin retired -
  retired runners stalled jobs in the queue).
- Adopted `clippy::duration_suboptimal_units` (new in 1.95);
  `Duration::from_secs(60)` rewritten to `from_mins(1)` across the
  tree.

### Fixes

- **Perf regression recovered.** The 0.6.0 GC work emitted a
  `gos_rt_gc_safepoint` call at every function prologue and every
  loop back-edge plus a `gos_rt_gc_root_save`/`_restore` pair around
  every function. The runtime calls are opaque to `opt -O3` and
  block inner-loop vectorisation; pure leaf math helpers (called
  > 10⁹ times in spectral-norm / n-body) paid the FFI cost on
  every invocation. The codegen now elides the prologue safepoint
  + shadow-stack save/restore when the body cannot allocate (new
  `gossamer_mir::body_might_allocate` helper) and drops the
  loop-back-edge safepoint outright - allocation routines update
  the byte-pressure counter and the next allocating function's
  prologue safepoint runs the collect when the threshold trips,
  which is sufficient for any body that grows the heap. Measured
  recovery in `gos build --release`.
- **HTTP server thread-per-connection restored.** 0.6.0 had
  swapped `gos_rt_http_serve` from "spawn a dedicated OS thread
  per accepted socket"  to "fixed worker pool + bounded
  `sync_channel`". With `available_parallelism() * 2` workers
  (≈ 48 on a 12-core box), > 48 concurrent clients saturated
  the pool, the queue filled, `try_send` started silently
  dropping sockets (RST'd by the OS), and the bench saw
  connection errors. The dedicated-thread shape (capped by
  `HTTP_ACTIVE_CONNS` / `GOSSAMER_HTTP_MAX_CONN` - default
  4096 - so a runaway client cannot bomb the thread / fd
  budget; past the cap responds 503 cleanly) is back.
- **Fuzz targets `hir_lower` + `vm_run` were broken on `cargo
  +nightly fuzz build`.** `grammar::render_source` was
  `pub(crate)` (invisible to fuzz-target bins, which are
  separate crates from the fuzz lib), and the call sites still
  used the pre-0.5.0 `parse_source_file(String, _)` /
  `vm.call(&str, &[])` signatures. Renamed to `pub`, swapped to
  `parse_source_file(&str, _)` / `vm.call(&str, Vec<Value>)`.
- `c_abi::tracing_gc_tests::ptr_key_is_send_sync_via_usize` now
  acquires `GC_TEST_LOCK`; previously raced the process-wide GC
  registry against sibling tests, producing intermittent
  "alloc_count = 0" / "freed = 0" failures.
- Removed unused `CloseHandle` import from `preempt.rs`
  (`-D warnings` failed the Windows build on Rust 1.95).

### Behavior changes

- Stricter at every IR boundary; some previously-silent miscompiles
  now refuse to compile.
- `gos build` is LLVM-only (Cranelift remains the in-process JIT for
  `gos`); `--release` runs the full `opt -O3 | llc -O3` pipeline.

## 0.5.1

### Bug fixes

- **`json::render(&adt)` now works in compiled mode.** Calling
  `json::render` on a user-defined struct previously fell through to the
  raw `gos_rt_json_render` path in compiled (Cranelift/LLVM) code,
  where the runtime misinterpreted the struct pointer as a `GosJson`
  Arc - crashing on the first field access.

- **Compiled-mode segfault when `json::render` appears in one branch of
  an if-else.** `lower_json_render_adt` allocates a `pairs_vec` (via
  `Vec::new`) only inside the JSON arm. `insert_drops_at_returns`
  scanned all blocks globally and emitted `gos_rt_vec_free(pairs_vec)`
  at every `Return` - including the other arm where `pairs_vec` was
  never initialised, producing `gos_rt_vec_free(0x21)` → segfault in
  `__GI___libc_free`.

## 0.5.0

### Language

- **Tree-walker retired.** `gos` now exclusively uses the register-based
  bytecode VM. The `--tree-walker` / `--vm` flags are removed; `gos` has
  no mode selector. Programs that previously required the walker fall back to
  the VM or should use `gos build`.
- **Generic structs.** `struct Pair<A, B> { fst: A, snd: B }` is typechecked
  across multiple instantiation sites. Per-instance substitution at field-read
  sites lets field arithmetic (`p.fst + p.snd`) resolve to the correct
  concrete type. Supported in the VM tier; compiled-tier parity tracked
  separately.
- **`extern "C" { }` rejected at parse time (GP0016).** Parser previously
  infinite-looped on any `extern` block. Fixed: the extern item is consumed
  cleanly and GP0016 is emitted. Applies to bare block,
  `#[no_mangle] extern "C" fn`, and `unsafe extern "C" { }` forms.
  `gos explain GP0016` directs users to `[rust-bindings]`.
- **`vec![...]` macro confirmed for 0.5.0.** `assert!`, `assert_eq!`,
  `debug_assert!`, `todo!`, `unimplemented!`, `write!`, `writeln!` are
  rejected at parse time (SPEC §14 not-in-0.5.0).

### VM / runtime

- **Call depth limit with clean diagnostic (GX0008).** Unbounded recursion
  now produces `error[GX0008]: stack overflow - call depth exceeded 40 frames`
  with a call-stack trace instead of a Rust stack overflow / SIGSEGV. The
  limit is calibrated for debug builds; `gos build` is not affected - native
  code uses the OS call stack. `gos explain GX0008` registered.

### Correctness

- **MIR verifier wired into optimization pipeline.** `verify_body` runs after
  every optimization pass. Structural drift (bad block ids, out-of-range
  locals, missing call targets) panics immediately under debug assertions
  instead of silently miscompiling.
- **GC write barriers emitted.** New shared `gossamer_mir::insert_gc_barriers`
  pass walks every projected pointer-store and emits
  `StatementKind::GcWriteBarrier`; both LLVM and Cranelift backends emit
  `gos_rt_write_barrier`. Concurrent collector is now safe as the default;
  `GOSSAMER_GC_MODE=stw` disables the allocation-driven incremental drive.
- **Race detector: multi-reader RAW/WAR tracking.** Per-address state now
  stores the last write and up to four concurrent active reads. Write accesses
  check all active readers for write-after-read conflicts; read accesses check
  the last write for read-after-write conflicts. Previous single-entry
  tracking missed races where a reader's record was overwritten before the
  conflicting write arrived.
- **LSP did-you-mean quick-fix.** `textDocument/codeAction` now surfaces
  machine-applicable `Suggestion` objects for unresolved-name diagnostics,
  not just help text. Editors that support quick-fixes receive a one-click
  rename to the nearest spelling match.
- **`ExprKind::Error` AST variant.** All compiler passes (HIR lower,
  typechecker, resolver, MIR lower, interpreter, LSP passes) handle the new
  `Error` expression variant. Malformed sub-expressions can now be represented
  in the AST instead of being silently dropped, enabling error-recovery paths
  that suppress cascading diagnostics.
- **Native codegen - zero LLVM fallbacks on `tier_parity`.** Aggregate
  Display formatting (Vec / Array / `JsonValue` / `DynError`) lowers inline
  via `gos_rt_*_format_*` helpers; struct-update aggregate-store path
  handles 1-slot fields; `Ok(struct)` heap-copies the aggregate so the
  payload pointer outlives the producer's frame; `gos_rt_chan_send`
  stack-spills its value arg; `channel()` materialises a fresh 16-byte pair
  buffer so `(tx, rx)` destructuring can't overflow a 1-slot alloca;
  `bitcast void` IR errors fixed.
- **Unary `Not` type inference.** MIR `lower_unary` inherits the operand's
  concrete type when the HIR result is `Var(_)`, fixing `!fs::exists(p)`
  segfaults where the `i1` result was being routed through `print_str`.

### Context cancellation

- **`rx.recv_ctx(&ctx)` end-to-end.** New runtime helper
  `gos_rt_chan_recv_ctx_option` plus cross-crate hook bridge
  (`gos_rt_install_ctx_hooks`); MIR dispatches the method name to the
  helper, and interp gains a matching `Channel::recv_ctx` builtin. OS-thread
  callers observe cancel within 50 ms via a bounded `wait_timeout`;
  goroutine callers via the scheduler's existing unpark path. Context flows
  in from any surface that hands one out (today: HTTP `r.context`).
- **Cancellation tests.** 4 channel-context tests, 3 net-context tests
  (`TcpListener::accept_ctx`, `TcpStream::read_ctx`).

### Tooling / CI

- **Miri nightly workflow.** `.github/workflows/miri.yml` runs `cargo miri
  test --lib` weekly against the seven safety-load-bearing crates (gc, mir,
  types, resolve, runtime, coro, sched).
- **Workspace lint debt.** `unsafe_code` workspace level changed `forbid`
  → `deny` so per-fn `#[allow(unsafe_code)]` works without each crate
  re-listing every workspace lint. Four of five unsafe-using crates dropped
  their duplicated `[lints]` overrides.
- **`tier_parity` flake fix.** New `PARITY_WALK_LOCK` serialises the
  cranelift/llvm parity walks so concurrent test functions can't race on
  shared `/tmp/gossamer_test_*` fixture paths.
- **`release_perf` tolerance fix.** Sub-50 ms wallclock skips the
  ratio check (both backends constant-folded the loop to startup-noise);
  live-loop tolerance bumped 1.10× → 1.25× for CI jitter.
- **Every bug-tracking `#[ignore]` closed.** 6 previously-ignored tests
  unblocked (channel drain, nested format precision, capturing closure as
  goroutine, `?`-through-indexed-Vec-field, 1k and 10k goroutine stress);
  the only remaining `#[ignore]`s are explicitly opt-in perf
  characterizations.

### Serialization safety

- **Depth and size limits for JSON, XML, and YAML.** Default: 128 levels deep,
  16 MiB. Pre-parse size rejection avoids allocation; depth is tracked live
  during parse. Process-wide overrides via `set_max_depth` / `set_max_size`.

### Fuzzing

- **7 fuzz targets.** `lex`, `parse`, `manifest`, `http_request`, `typecheck`,
  `mir_lower` (includes verifier), `vm_compile`. 30-second smoke CI on every
  PR; 1-hour weekly deep run.

### Perf CI

- **Baseline-pinned regression gate.** Per-benchmark baselines are cached
  between CI runs; any benchmark that exceeds 2× its baseline fails the build.
  Three representative programs exercise arithmetic, recursion, and I/O on
  every PR.

### SPEC conformance tests

- 9 tests in `spec_conformance` pin every 0.5.0 conformance banner
  behaviorally: GP0016 rejection, macro subset, integer overflow no-panic,
  borrow-check not enforced, `--message-format json` schema.

### Edge-case tests

- 3 tests in `edge_case_battery`: NaN propagation, double-close channel panic,
  stack-overflow → GX0008. All use spawn + timeout so they cannot seize CI.

## 0.4.0

### Stdlib reorganization (Rust-style `fs` / `env` / `process`, Go-style HTTP/2)

The standard library's process-level surface was restructured for
intuitiveness. Filesystem ops moved out of `os`, environment +
argv split into `env`, child processes into `process`. HTTP/2 was
dissolved into `std::http` exactly as Go does in `net/http` - no
separate `std::http2` namespace.

**New modules:**

- **`std::env`** - `args`, `program_name`, `var`, `set_var`,
  `unset_var`, `current_dir`, `set_current_dir`, `home_dir`,
  `temp_dir`. Mirrors Rust's `std::env`.
- **`std::process`** - `Command`, `Output`, `Stdio`, `ExitStatus`,
  `Child`, `run`, `spawn`, `kill`, `exit`, `id`, `abort`. Mirrors
  Rust's `std::process`.

**Expanded `std::fs`** with the full filesystem surface, no longer
sparse: `read`, `read_to_string`, `write`, `read_dir`, `walk_dir`,
`create_dir`, `create_dir_all`, `remove_file`, `remove_dir`,
`remove_dir_all`, `remove_all`, `copy`, `rename`, `exists`,
`is_file`, `is_dir`, `is_symlink`, `file_size`, `metadata`,
`canonicalize`, `glob`, `eval_symlinks`. `fs::is_file`,
`fs::is_dir`, `fs::is_symlink`, `fs::file_size` are wired through
the compiled tier with new `gos_rt_os_is_symlink` /
`gos_rt_os_file_size` runtime helpers.

**HTTP/2 folded into `std::http`** (Go-style). `std::http2` is
gone. Renamed entry points live under `std::http`:

| Old (`std::http2::*`) | New (`std::http::*`) |
| --- | --- |
| `bind_and_run_h2c` | `serve_h2c` |
| `bind_and_run_h2c_streaming` | `serve_h2c_streaming` |
| `serve_connection` | `serve_h2_connection` |
| `serve_connection_streaming` | `serve_h2_connection_streaming` |
| `Handler` | `Http2Handler` |
| `StreamingHandler` | `Http2StreamingHandler` |
| `ResponseWriter` | `StreamingResponseWriter` |
| `Config` | `Http2Config` |
| `ServerHandle` | `Http2ServerHandle` |
| `Error` | `Http2Error` |

**`std::path` is now I/O-free.** `path::walk` was removed
(`fs::walk_dir` is canonical); `glob` and `eval_symlinks` moved to
`fs::glob` / `fs::eval_symlinks`.

**`std::os` shrunk to OS identity.** New: `os::family()`
(`"unix"`/`"windows"`), `os::arch()` (CPU triple component). The
old filesystem/env/process functions stay callable for one minor
release as deprecated re-exports - every entry in the `os::`
manifest now says "Deprecated: use ...".

**New documented modules:** `std::log` (Go-style flat log shape)
and `std::thread` (native OS threads) both existed in source but
were absent from the manifest; both now documented.

**Naming aliases (no behavior change):**

- `strings::to_lower` / `to_upper` - short alias for
  `to_lowercase` / `to_uppercase`, matching SKILL.md and Go.
- `strconv::parse_int` / `atoi` / `parse_float` / `format_int` /
  `itoa` / `format_float` - Go-style aliases for the existing
  `parse_i64` / `parse_f64` / `format_i64` / `format_f64`.

**Manifest dedup:** the split `ENCODING_BINARY` /
`ENCODING_BINARY_FULL` entries collapsed into a single
`std::encoding::binary` block.

**Dropped bare-module aliases:** `gzip::*` was a back-compat alias
for `compress::gzip::*` - removed; the canonical path was already
the dispatch shape every example used. Bare `exec::*` retained
for back-compat alongside the new `process::*`.

**Migration:** `docs_src/migration/rust.md` and
`docs_src/migration/go.md` now ship a "Standard library mapping"
table each, calling out the Rust → Gossamer and Go → Gossamer
shape of every common entry. `examples/cat.gos`, `grep.gos`,
`environment.gos`, `cli_args.gos`, `simple_cli_args.gos`,
`list_dir.gos`, `http2_server.gos`, and
`projects/web_service_full/src/main.gos` all rewritten to the
canonical names.

A new `stdlib_surface_snapshot` regression test in
`crates/gossamer-std/tests/` pins the documented item count so
future drops require a deliberate floor adjustment.

### Binding ABI

Four new shapes in the Rust-binding system; every 0.3 binding crate
recompiles unchanged.

- **`Type::Bytes`** - first-class byte payload, distinct from
  `Vec<i64>` at the source level. Rust shape is the new
  `gossamer_binding::Bytes` newtype (transparent `Vec<u8>`).
  Compiled tier uses a `GosBytes { len, cap, ptr }` C-ABI struct;
  interp tier stores as `Value::IntArray`.
- **`Type::Map<K, V>`** - keyed collection backed by `HashMap<K, V>`.
  Compiled tier uses `GosMap { keys, values }` parallel-vec headers.
  Concrete impls for `HashMap<String, String>`,
  `HashMap<String, i64>`, `HashMap<i64, i64>`.
- **`Type::Variant<arms...>`** - tagged-union return backed by the
  new `gossamer_binding::DynValue` enum (Nil, Bool, Int, Float,
  Char, String, Bytes, List, Map, Tagged). Compiled tier uses
  `GosDynVariant { name, payload_len, payload }` with arena-
  allocated arm names.
- **`Type::Callback(args, ret)`** - Gossamer-side callable that
  bindings may invoke during their call. `BindingCallback` for
  interp (wraps a `Value`), `NativeCallback` for compiled (wraps a
  `u64` handle). Lifetime is strictly call-scoped - retaining past
  the binding return is undefined behaviour.

`gossamer_resolve::BindingType`, `gossamer_driver::DumpedType`,
`gossamer_runner_template/sigs_dump.rs.tmpl`, and
`gossamer_mir::lower::binding_type_to_mir` all extended to handle
the new shapes. Architecture spec at
`crates/gossamer-binding/ABI_0_4.md`.

### CI test reliability

- **Port-bind race in HTTP tests fixed.** The `pick_port()` helper
  in `gossamer-std`'s `http_server`, `http_proxy`, and
  `http_native_client` test modules bound `127.0.0.1:0`, read the
  assigned port, **dropped the listener**, then expected the test
  to re-bind the same port. On Windows CI agents and busy hosts
  the gap was reliably exploited, producing intermittent
  `AddrInUse` panics and `gossamer-std --lib` / `--test http_server`
  failures with exit code 101. Replaced with `bind_loopback() ->
  (TcpListener, SocketAddr)` that hands the live listener back.

### Language / parser

- **Statement boundary for leading `&` / `*` / `-`.** A newline
  followed by one of these three operators now ends the previous
  statement, so `let s = read(p)?\n&s |> ...` parses as two
  statements instead of `let s = read(p)? & s |> ...`. Multi-line
  continuation still works when the operator sits at the end of
  the previous line (`let x = a -\n  b`) or inside parentheses;
  all other binary operators continue across newlines
  unconditionally. SPEC §2.7.
- **`?` in macro argument position propagates early-return.**
  `print!("{}", expr?)` correctly returns `Err(e)` from the
  enclosing function when `expr` is `Err`; previously the result
  was silently passed to `__concat`.

### Manifest

- **Explicit `[[bin]]` and `[lib]` tables in `project.toml`.**
  Array-of-tables for `bin`, single-table for `lib`. Duplicate
  bin names rejected. Implicit filesystem convention
  (`src/main.gos` / `src/lib.gos`) still works when neither is
  present.

### HTTP - wire correctness

- **`Client::builder().tls(...)` and `.cookies(...)` now work.**
  Previous behaviour silently dropped both. `ClientConfig` retains
  the source PEM bytes so the ureq bridge can rebuild TLS state.
- **`Date` and `Server` headers auto-inserted on every response**
  per RFC 9110 §6.6.1. `Server` value is configurable via
  `Config.server_name` (default `gossamer/0.4.0`); handler-supplied
  `Date` / `Server` headers are preserved without duplication.
  New `std::time::format_rfc1123_gmt` helper.
- **Chunked transfer encoding** (RFC 7230 §4.1) for both inbound
  request bodies and outbound responses. New `std::http_chunked`
  module (`ChunkedReader` + `ChunkedWriter`) with malformed-input
  hardening (bad hex, premature EOF, missing CRLF, oversize
  length, chunk-extensions). Trailer headers on inbound chunked
  bodies merge into `request.headers`. Outbound chunked is
  triggered by the handler setting `Transfer-Encoding: chunked`;
  `Content-Length` is stripped when both are present. Combination
  of chunked + `Content-Length` on the request is rejected with
  `400`.
- **`Expect: 100-continue`** support (RFC 7231 §5.1.1) on both
  plain-TCP and TLS paths. The HTTP parser is split into
  `parse_request_head_generic` and `finish_request` so the server
  can write the interim response between head parse and body read.
- **Path / query split.** `Request.path` is now the URL path alone
  (Go's `URL.Path` semantics); `Request.query` carries the raw
  query string (no leading `?`). New helpers: `Request::query()`,
  `Request::request_uri()`, `Request::query_pairs()` (percent-
  decoding), and `std::http::split_path_query()`.
- **`Headers::remove(name)`** added.
- **Unified HTTP/1.1 parser.** The TLS and plain-TCP paths now
  share a single generic `parse_request_head_generic` +
  `finish_request` implementation.

### HTTP - timeouts and graceful shutdown

- **Timeout taxonomy.** `Config` gains `read_header_timeout`
  (10 s default, slowloris guard), `read_body_timeout` (30 s),
  `write_timeout` (30 s), `idle_timeout` (75 s). The legacy
  `read_timeout` knob still works as a blanket fallback. Per-phase
  deadlines enforced via `Instant`-based total-elapsed checks in
  the parser and body reader; per-syscall timeouts via
  `set_read_timeout` / `set_write_timeout` switching across the
  idle → header → body → write phases.
- **`Server::shutdown(&Config, Option<Duration>) -> bool`** -
  flips the shutdown flag, blocks until `Config.in_flight` drains
  to zero or the deadline elapses. Returns `true` on clean drain,
  `false` on timeout. Worker loop polls the flag between
  keep-alive requests so idle connections close promptly.
- **Per-request `Context` cancellation.** A watcher fires the
  cancel handle when `Config.shutdown` trips, so long-running
  handlers observe `request.context().is_cancelled() == true`.

### HTTP - router, middleware, static files, proxy

- **`http_router`** - Go 1.22-class `ServeMux` with `{name}`
  captures, `{rest...}` trailing captures, `*` wildcard, method
  gating (`get` / `post` / `put` / `delete` / `patch` / `head` /
  `options`). Precedence: method-specific beats method-agnostic;
  more-specific pattern wins; insertion-order breaks ties. Default
  404 / 405 responses with overridable hooks.
- **`http_middleware`** - Logger, Recoverer
  (`std::panic::catch_unwind`), RequestId (`X-Request-Id` stamping
  with carry-through), CORS (preflight + per-response headers),
  BasicAuth (RFC 7617), Compress (gzip body framing gated on
  `Accept-Encoding`, with min-bytes threshold).
- **`http_static_files`** - `FileServer` with configurable `etag`,
  `last_modified`, `range_support`, `max_file_bytes`. Path-
  traversal guard (`fs::canonicalize` + prefix check). 200 / 206
  / 304 / 404 / 416 response shaping. ETag from `mtime + size`,
  RFC 1123 GMT `Last-Modified`. MIME table covers 25 common
  extensions. `index.html` auto-served on directory hits.
- **`http_proxy`** (behind `http-client` feature) - `ReverseProxy`
  with caller-supplied `director`, `modify_response`,
  `error_handler`. `ReverseProxy::single_host` forwards path +
  query verbatim. Hop-by-hop header stripping per RFC 7230 §6.1.
  Auto-appends `X-Forwarded-For`, `X-Forwarded-Host`,
  `X-Forwarded-Proto`.

### HTTP - WebSocket and SSE

- **`http_websocket`** - RFC 6455 from scratch. `accept` performs
  the handshake (validates Upgrade / Connection /
  Sec-WebSocket-Version=13 / Sec-WebSocket-Key, computes
  Sec-WebSocket-Accept via inline SHA-1 + base64). `WebSocket`
  exposes `send_text` / `send_binary` / `send_ping` / `send_pong`
  / `send_close` / `receive` over any `Read + Write` stream.
  Auto-pong on inbound ping. Fragmented frame reassembly via
  continuation opcodes. Server-mode requires client masking;
  client-mode masks outbound frames. Length encoding handles
  7-bit, 16-bit, and 64-bit forms. Inline SHA-1 + base64
  implementations (no extra deps).
- **`http_sse`** - Server-Sent Events (`text/event-stream`)
  encoder: `SseStream::send` (event name / id / data lines),
  `send_retry`, `send_comment` (heartbeat). `event_stream_headers()`
  + `response_skeleton()` helpers.

### HTTP/2 server

- **`std::http::serve_h2c`** in both `gos` and `gos build`.
  (Renamed from `std::http2::bind_and_run_h2c` during 0.4.0 dev -
  HTTP/2 is now folded into `std::http` per the Go model; see
  "Stdlib reorganization" above.) The `h2` crate runs on
  Gossamer's own goroutine scheduler via `runtime_future::drive`
  (a future-pump) + `async_tcp::AsyncTcpStream` (mio-bridge over
  non-blocking TCP). Tokio is consumed only for its `AsyncRead` /
  `AsyncWrite` trait surface.
- Bounded `Http2Handler` (`fn serve(req) -> Response`) and chunked
  `Http2StreamingHandler` (`fn serve(req, StreamingResponseWriter)`)
  shapes both supported. `StreamingResponseWriter::write_chunk`
  flushes the response head on first call and emits one `DATA`
  frame per call; `finish` (or `Drop`) sends the terminating
  `END_STREAM`.
- **ALPN-driven HTTPS dispatch** via `bind_and_run_tls_h2`
  (tokio-rustls trait-only).
- Architecture documented at `crates/gossamer-std/HTTP_H2_ARCH.md`.

### Native HTTP/1 client

- **`http_native_client`** built on `std::net::TcpStream`.
  `NativeClient::{get, post, put, delete, request}` with per-
  client connection pool (keyed by host:port), configurable
  redirect policy (default 10 hops), chunked response decoding,
  user-agent / timeout / max-body-bytes config. HTTPS not yet
  supported; TLS stays on the existing ureq path.

### HTTP module bridges - interp + compiled parity

Eight stdlib HTTP modules now callable from Gossamer source in
both tiers, byte-identical across `gos` and `gos build`.

- **router / FileServer / NativeClient / Proxy** - stateful,
  method-chain dispatch. `Router::new()`, `r.get(path, Handler {})`,
  `r.serve(req)` and the rest of the verb chain work end-to-end.
  22 new `gos_rt_*` runtime symbols. MIR auto-synthesises
  `gos_fn_addr("{Handler}::serve")` for HTTP-verb methods so the
  runtime can transmute and invoke user handlers through the same
  fn-pointer ABI as `gos_rt_http_serve`. `gos_fn_addr` now
  resolves to `intrinsics.externs` for runtime symbols.
- **chunked / sse / middleware / websocket-accept-key /
  static_files-mime** - stateless free-fn shapes.
  `chunked::encode` / `decode`, `sse::encode_event` / `comment` /
  `retry`, `middleware::new_request_id` / `accepts_gzip`,
  `websocket::accept_key`, `static_files::mime_for_path`. Self-
  contained SHA-1 + base64 in the runtime for the WS accept-key
  derivation.
- **MIR runtime-kind tag from rendered type** - `lower_fn`'s
  parameter binding now reads the rendered type of the param (in
  addition to the binding name), so `r: http::Request` resolves
  the same as `request: http::Request`. Fixes garbage reads on
  `r.path` / `r.body` for handler params named anything other than
  `request` / `req`.

### Netpoller latency

- Tightened the `globals().poller.lock()` hold during
  `mio::Poll::poll()` so registering goroutines no longer wait up
  to 50 ms per IO op. New `mio::Waker` interrupts in-flight polls
  when `with_poller` mutates state; poll cycle dropped to 1 ms.
  Multiplexed h2c: 3.7 ms/req, was 100 ms.

### Networking

- **`net::TcpStream::set_keepalive(Option<Duration>)`** - socket2-
  backed `SO_KEEPALIVE` toggle.
- **`net::TcpStream::connect_happy_eyeballs(addrs, stagger,
  timeout)`** - Go 1.21-style v6/v4-interleaved race with per-
  candidate staggered start.
- **`net::UnixListener` / `net::UnixStream`** (Unix-only) - bind /
  accept / connect / read / write / shutdown.
- **`net::IpNet`** - RFC 4632 prefix parsing for IPv4 and IPv6,
  `contains(&Ip)` predicate, `prefix_len()`, `render()`. Cross-
  family addresses are rejected.
- **`net::url`** - `path_escape` / `path_unescape`,
  `UserInfo { username, password: Option<String> }` (parse +
  render with percent-encoding), `Values` (Go's `url.Values`:
  `add` / `set` / `get` / `get_all` / `delete` / `encode` /
  `parse`).
- New `socket2 = "0.5"` dependency in `gossamer-std`.

### Stdlib

- **`std::io`** - `copy`, `copy_n`, `read_all`, `LimitReader`,
  `TeeReader`, `MultiReader`, `pipe` (paired `PipeReader` +
  `PipeWriter` with cross-thread blocking semantics).
- **`std::log`** (new) - Go-style flat logger: `println`,
  `printf`, `fatal`, `panic_msg`; `set_output`, `set_prefix`,
  `set_flags`; flag constants `L_DATE`, `L_TIME`, `L_MICROSECONDS`,
  `L_JSON`, etc. Global process-wide sink protected by
  `parking_lot::Mutex`.
- **`std::time`** - `Ticker` (recurring callback every interval;
  `stop()` / Drop-safe), `after_func` (one-shot timer returning a
  cancellable `TimerHandle`), `SystemTime::from_std` / `as_std` /
  `unix_seconds`.
- **`std::sync`** - `SyncMap<K, V>` (read-heavy `RwLock`-backed
  concurrent map: `store` / `load` / `load_or_store` / `delete` /
  `contains` / `range`), `Pool<T>` (factory-backed freelist),
  `Cond` (`parking_lot::Condvar` wrapper for `signal` /
  `broadcast` / `wait`).
- **`std::path`** - Go `path/filepath` parity: `glob(pattern)`
  (literal, `*`, `?`, `[class]`, `**` recursive), `matches(pattern,
  name)` segment matcher (no `/` crossing), `walk(root, visit)`
  with `SKIP_DIR` / `SKIP_ALL` sentinels, `eval_symlinks(path)`.
- **`std::crypto::cipher`** - `aes_ctr_xor` (in-place encrypt/
  decrypt for 128/192/256-bit keys), `aes_cbc_encrypt` +
  `aes_cbc_decrypt` with PKCS#7 padding. Bad key sizes and bad
  IVs return typed errors.
- **`std::runtime`** - `caller(skip)` returns
  `Option<StackFrame>`, `stack()` returns `Vec<StackFrame>` (both
  backed by the `backtrace` crate). `set_finalizer(arc, fn)`
  returns a `Finalizer<T>` guard with `cancel()` and Arc-aware
  drop semantics - fires only when the last clone goes away.
- **`std::text::template`** - `FuncMap` registry with default
  helpers (`upper`, `lower`, `trim`, `len`, `default`,
  `html_escape`); pipelines (`{{ .x | upper | trim }}`);
  `Template::render_with_funcs(data, funcs)` and free
  `render_with_funcs(source, data, funcs)`. Unknown function names
  raise `Error::Parse`.

### Stdlib feature gates removed

`gossamer-std` no longer ships behind feature flags - every
module (regex, tls, crypto, compress, archive, http2, templates,
sql, ureq, …) is unconditionally compiled. The `[features]`
table is gone; consumers depend on the crate plain. 58
`#[cfg(feature = …)]` sites stripped.

### Tooling

- **`gos doc --emit-stdlib DIR`** - walks `manifest::ALL_MODULES`
  and emits one Markdown page per module under `DIR` plus an
  `index.md` landing page. `--check` mode compares disk against
  generated output and fails the build on drift. Wired into
  `check.sh` and the `stdlib-docs-drift` GitHub Actions job. 79
  stdlib pages committed under `docs_src/stdlib/`.

## 0.3.0

### Added

- **`std::compress` expanded.** New `flate` (raw DEFLATE), `zlib`, and
  `bzip2` submodules join the existing `gzip` module. All three are
  feature-gated (`compress` / `bzip2-compress`).
- **`std::archive`.** New `tar` and `zip` submodules for reading and
  writing archives, backed by the `tar` and `zip` crates.
- **`std::hash::fnv`.** FNV-1a and FNV-1 hashes in 32- and 64-bit
  variants; no new dependencies.
- **`std::encoding` expanded.** New `base32` (RFC 4648 standard and hex
  alphabets), `ascii85` (Adobe / btoa), and `xml` (quick-xml backed)
  submodules. Qualified-path access for `encoding::base64` and
  `encoding::hex` is now wired.
- **`std::crypto::insecure`.** MD5 and SHA-1 for legacy-compatibility
  contexts; feature-gated as `insecure-crypto`.
- **`std::math::big`.** Arbitrary-precision integers via decimal-string
  representation. Exposes `Int::parse`, `Uint::parse`, `Int::compare`,
  and `factorial`.
- **`std::sync::AtomicU64` and `sync::Barrier`** wired to the interpreter.
- **52 integration tests** for all new stdlib modules in
  `crates/gossamer-cli/tests/stdlib_new_modules.rs`.
- **5 new examples**: `crypto_hashing.gos`, `encoding_codecs.gos`,
  `big_numbers.gos`, `compress_demo.gos`, `html_escape.gos`.

### Performance

- **Parallel Cranelift body lowering.** Function bodies now compile
  concurrently via rayon. An `OfflineModule` snapshot pre-declares all
  symbols in a single-threaded phase; each rayon worker then lowers its
  assigned body without taking any global lock.
- **Auto-drop pass overhaul.** Ten stacked compiler and runtime fixes
  make the heap-free pipeline produce IR that actually executes
  `gos_rt_*_free` calls. Changes include: per-block liveness-based drop
  placement, copy-alias chain tracking, inter-procedural escape analysis
  (`CaptureSummary`), a sentinel-pointer skiplist for globally-owned
  buffers, and `gos_rt_str_free` for owning strings. Effect on benchmarks
  (source unchanged): k-nucleotide −33% peak RSS, spectral-norm −8%.
- **`gos test` parallel by default.** Defaults to
  `available_parallelism()` workers. `--serial` (alias `--parallel 1`)
  opts back to sequential execution.
- **`define_only` allow-list check is O(1).** Converted from a linear
  scan to a `HashSet` in `lower_program_full`.

### Architecture

- **Incremental GC drive wired into the allocation fast path.**
  `gos_rt_gc_alloc_rooted` calls `drive_incremental()` after each
  rooted allocation: starts a new concurrent cycle when RSS exceeds the
  threshold (default 4 MB; override with `GOSSAMER_GC_TARGET`), marks a
  32-object batch during marking, and finalises when the grey set is
  exhausted.

### Fixes

- **`Result` used as a bare statement is now a compile error (GT0007).**
  Every `Result<T, E>` expression must be handled via `?`,
  `match`/`if let`, or `let _ = expr`. `gos explain GT0007` documents
  the rationale.

## 0.2.0

### Performance

- **JIT peak RSS reduced for programs with large array initialisers.**
  - `Rvalue::Repeat` in the Cranelift backend now skips all stores for
    zero-constant fills (`[0.0; N]`, `[false; N]`, `[(); N]`) - `calloc`
    already zeroes memory, so the stores were redundant. Non-zero fills
    larger than 16 elements emit a counted loop (O(1) IR) instead of N
    unrolled `store` instructions (O(N) IR), matching the LLVM backend.
  - Array-typed return values in the Cranelift backend are now returned
    directly (the existing `calloc`-allocated local is passed back as-is)
    instead of going through a second `gos_rt_gc_alloc` + memcpy escape.
    Saves one allocation per array-returning call.
  - JIT compilation now pre-filters to the minimal set of bodies needed:
    a BFS from JIT-promotable roots (functions with scalar-only
    param/return types) collects their transitive user-function callees.
    Bodies that can never be promoted (aggregate params/returns) are
    skipped entirely, cutting JIT compile time proportionally.
- **HIR and type-context dropped before `vm.call()`.** The CLI's `gos`
  path now explicitly drops the `HirProgram` and `TyCtxt` before entering
  the main call, then releases the MIR/TyCtxt JIT prelude after `vm.call()`
  returns and before goroutine-join. Frees the per-program compilation data
  while goroutines are still running, reducing peak RSS for large programs.

### Architecture

- **ABI registry (`gossamer-abi` crate) for typed `gos_rt_*` declarations.**
  A new `gossamer-abi` crate holds a single source-of-truth for every
  `gos_rt_*` symbol's name and C-ABI signature. The Cranelift backend's
  `extern_fn_by_name` and the LLVM lowerer's `declare_rt` both derive
  function declarations from this registry, eliminating the previously
  parallel string arrays. Typos in symbol names now panic at test time
  rather than silently producing wrong code.

### Fixes

- **LLVM write-barrier correctness.** The write barrier was being emitted
  for `ptr`-typed LLVM values (raw machine pointers). `gos_rt_write_barrier`
  expects a `u32` GcRef index (widened to i64 in the flat ABI); truncating
  a pointer to i32 is both invalid IR and semantically wrong. The barrier
  is now suppressed for all `ptr`-typed values; the GC tracks those through
  its allocation registry.
- **LLVM aggregate-return memcpy for runtime helpers.** When a runtime call
  returns a heap pointer to a multi-slot aggregate (e.g.
  `gos_rt_result_payload` returning an `ExecOutput` blob), the destination
  is an inline `[N x i64]` alloca. A bare `store ptr` only wrote the blob
  address into slot 0, making subsequent field reads load the blob pointer
  instead of the actual field value. The LLVM lowerer now emits a full
  memcpy for these cases.
- **LLVM call-site type declarations match the call instruction.** Runtime
  functions whose registry ABI type differs from the LLVM call-site type
  (e.g. `gos_rt_result_payload` is `I64` in the registry but called as
  `ptr` in compiled MIR) now always declare using the call-site type.
  Registry-type declarations caused `opt` to miscompile the wrong type.

### Added

- **PGO support for `gos build --release`.**
  Two environment variables drive a standard three-step LLVM PGO workflow:
  - `GOS_PGO_COLLECT=<output.profraw>` builds an instrumented binary that
    writes raw profile data on exit. Links `libclang_rt.profile-x86_64.a`
    automatically.
  - `GOS_PGO_PROFILE=<merged.profdata>` feeds a previously collected and
    `llvm-profdata`-merged profile into `opt --pgo-kind=pgo-instr-use-pipeline`.
  The `gos build` command prints the three-step workflow on first use.
- **Binary size reduction for `gos build`.**
  Release builds now strip all symbols and dead sections (`-Wl,--gc-sections`
  on Linux, `-dead_strip` on macOS). Debug builds without `-g` strip only
  debug sections, keeping symbol names for crash reports. Brings the
  Cranelift-generated binary floor down ~75%.
- Github Actions tests do not fail fast.
## 0.1.8

### Fixes

- **`STATUS_HEAP_CORRUPTION` crash in native iterator test on Windows.**
  The MIR drop-insertion pass pins the destination local of `gos_rt_arr_iter`
  to the source `Vec<T>` type so `.next()` dispatch can recover the element
  kind. The type-based `inferred_free` path then incorrectly scheduled
  `gos_rt_vec_free` on the `*mut GosArrIter` pointer, interpreting the
  iterator's raw bytes as a `GosVec` header and corrupting the heap on free.
  Fixed by adding `gos_rt_arr_iter_free` to the runtime and registering
  `"gos_rt_arr_iter" => "gos_rt_arr_iter_free"` in `ctor_to_free`, so the
  drop pass emits the correct free instead of `gos_rt_vec_free`.

- **Missing `.exe` suffix on Windows in two multi-file regression tests.**
  `cross_file_project_bundles_sibling_modules` and
  `cross_file_chained_sibling_module_calls` constructed expected binary paths
  as bare stems (`target/debug/probe`, `target/debug/chained`) without the
  `.exe` extension. Fixed with `set_extension(EXE_EXTENSION)`, matching the
  pattern used in `parity.rs`.

- **Missing LLVM declaration for `gos_rt_arr_iter_free`.**
  The `dispatch_parity` test enforces that every symbol exported from
  `c_abi.rs` has a matching `declare` line in the LLVM prelude. Added
  `declare void @gos_rt_arr_iter_free(ptr)` to `gossamer-codegen-llvm/src/emit.rs`.

- **Directory sizes report 0 in native tiers on Windows.**
  `gos_rt_fs_list_dir` and `gos_rt_fs_walk_dir` used `DirEntry::metadata()`
  which reads from `WIN32_FIND_DATA` - a cached struct that stores
  `nFileSize = 0` for directories. The interpreter uses
  `std::fs::metadata(path)`, which opens a file handle and calls
  `GetFileInformationByHandle`, returning the real NTFS directory-index
  size. Both native functions now use `std::fs::metadata` to match.

- **Missing `.exe` suffix in `codegen_correct` and `native` integration tests on Windows.**
  `every_correct_program_matches_across_tiers` checked for binary artifacts at
  `target/debug/<stem>` and `target/release/<stem>` without the `.exe` extension,
  causing 16 failures (8 programs × 2 profiles). All 13 `gos build`-driven binary
  path constructions in `codegen_correct.rs` and `native.rs` now use
  `set_extension(EXE_EXTENSION)` or the new `debug_bin(&dir, stem)` helper.
  `gos_binary()` in `codegen_correct.rs` (the release `gos` tool path) is also fixed.

## 0.1.7

### Fixes

- **`exec::kill` interpreter implementation no longer uses unsafe on Windows.**
  `gossamer-interp` has `#![forbid(unsafe_code)]`, so the Win32 FFI approach
  from 0.1.6 was rejected at compile time. Replaced with `taskkill /F /PID
  <pid>` via `std::process::Command` - the same safe shell-out pattern used
  for `/bin/kill` on Unix. The compiled-tier runtime (`c_abi.rs`) keeps the
  direct `OpenProcess`/`TerminateProcess` approach, which is correct there
  since `gossamer-runtime` permits unsafe.

## 0.1.6

### Fixes

- **`unsafe extern` required in Rust 2024 edition.**
  The `extern "system"` blocks added in 0.1.5 for the Windows
  `exec::kill` implementation must be `unsafe extern "system"` in
  edition 2024. Fixed in both `gossamer-runtime/src/c_abi.rs` and
  `gossamer-interp/src/builtins.rs`.

## 0.1.5

### Fixes

- **`exec::kill` now terminates processes on Windows.**
  Both the compiled-tier runtime (`gos_rt_exec_kill` in `c_abi.rs`) and the
  interpreter (`builtin_exec_kill` in `builtins.rs`) returned `false`
  unconditionally on Windows. Both now call `OpenProcess(PROCESS_TERMINATE)`
  + `TerminateProcess` + `CloseHandle` via inline `extern "system"`
  declarations (no new dependencies). The `#[cfg(not(unix))]` fallback is
  split into `#[cfg(windows)]` (real implementation) and
  `#[cfg(not(any(unix, windows)))]` (stub for other platforms).

## 0.1.4

### Fixes

- **Test binary paths now include `.exe` on Windows.**
  Integration tests constructed expected output paths with bare stem names
  (`"agg"`, `"concurrent"`, etc.) but `gos build` correctly emits `<stem>.exe`
  via `platform_exe_name`. Fixed by appending `std::env::consts::EXE_SUFFIX`
  at every call site across seven test files (`aggregate_print_fallback`,
  `cli`, `format_precision_parity`, `memory_growth_bounded`, `parity`,
  `release_stability`, `stdout_concurrent_print`).


## 0.1.3

### Fixes

- **`gos build` now produces a `.exe` binary on Windows.**
  `output_path` and `resolve_output_path` were appending the bare unit name to
  the output directory on every platform. `rust-lld -flavor link` (unlike
  classic MSVC `link.exe`) writes the binary at the exact `/OUT:` path given,
  with no automatic `.exe` suffix. The result was a binary with no extension
  that `is_executable` on non-Unix (which checks for `.exe`) could not find,
  causing all `aggregate_abi` (and related) test cases to report
  "no binary in … \cl" on Windows CI. Fixed by adding a `platform_exe_name`
  helper in `paths.rs` that appends `.exe` on Windows, used consistently in
  both the `--out-dir` fast path and the default `target/{debug,release}/`
  path.

## 0.1.2

### Fixes

- **`os_env_compiled` test helpers no longer trigger dead-code warnings on Windows.**
  `os_set_env_round_trips_through_os_env_in_all_tiers` had an unnecessary
  `#[cfg(unix)]` guard - the test body is pure env-var I/O and runs on all
  platforms. Windows variant of the child-propagation test added using
  `cmd /c set`.

## 0.1.1

### Fixes

- **`exec_spawn` test helpers no longer trigger dead-code warnings on Windows.**
  All helper functions were ungated; Windows-equivalent test variants added
  (`ping 127.0.0.1` in place of `/bin/sleep`) so `exec::spawn` / `exec::kill`
  coverage runs on both platforms.

## 0.1.0

### Fixes

- use std::process::{Command as StdCommand, Stdio as StdStdio} inside 
  builtin_exec_kill is now #[cfg(unix)]-gated, since those aliases are only 
  used inside the #[cfg(unix)] block - dead warnings on Windows.
- **`Ok(N)` / `Some(N)` payload-literal matching in compiled mode.**
  `match r { Ok(1) => …, Ok(2) => … }` always took the first `Ok` arm
  because MIR only compared the discriminant, never the payload value.
  Now ANDs a `gos_rt_result_payload`-extracted value predicate with the
  disc predicate. Applies to all non-binding, non-wildcard payload
  patterns: literals, ranges, nested variants, or-patterns.
- **LLVM prelude missing `gos_rt_arr_iter`, `gos_rt_arr_iter_next`,
  `gos_rt_json_set`.** Three helpers declared in `c_abi.rs` had no
  `declare` entry in the LLVM IR prelude; they silently linked to zero.
  Added declarations; `dispatch_parity` test now enforces this for all
  future helpers.
- **`Option<T>` discriminator regression in compiled tiers fixed.**
  `match json::get(...)` returning `Some(v)` matched neither
  `Some` nor `None` arms - both fell through silently. Root cause:
  the runtime helpers `gos_rt_json_get`, `gos_rt_json_keys`,
  `gos_rt_json_as_array` returned bare `*mut GosJson`/`*mut GosVec`
  pointers, but user-level `json::get` is typed as `Option<&Value>`
  so MIR expected an Option-shaped `*mut GosResult` (16 bytes:
  `disc: i64, payload: i64`, `disc == 0` = Some, `disc == 1` =
  None). Added three new opt-flavoured helpers
  (`gos_rt_json_get_opt`, `gos_rt_json_keys_opt`,
  `gos_rt_json_as_array_opt`); MIR routes user-level json calls
  through them while internal field-access lowering keeps the
  bare helpers. Interp tier wraps the same calls via
  `some_variant(...)` / `none_variant()`. The bare helpers stay
  for chained MIR field-projection (`root.a.b.c`) so the wrap
  cost only lands on user-visible Option boundaries. Tests
  `json_get_returns_option_with_correct_discriminator`,
  `malformed_json_returns_none_not_segfault`,
  `json_as_array_iter_native` all pass now.
- **Cranelift native codegen - nested struct field offsets.**
  `o.inner.x` segfaulted: field projections used flat `idx*8`
  offsets that ignored embedded struct widths. Rewrote
  `lower_place_address` / `resolve_place_cl_type` /
  `resolve_place_ty` to sum `type_slot_count` of preceding
  fields. `Aggregate` construction (struct + tuple) now uses
  per-field widths from `tcx.struct_field_tys` and walks the
  nested layout. Also added a projected-aggregate-read
  shortcut so a slot returns the field address rather than
  collapsing to first slot. Same flat-`idx` bug fixed in the
  LLVM lowerer.
- **Cranelift call-site struct alias bug.** Multi-slot
  aggregates passed by value aliased the caller's storage
  (`shift(p)` mutated `original`). Added defensive copy via
  new `operand_aggregate_slots` / `clone_aggregate_value`
  helpers - fresh storage + per-slot memcpy at every
  by-value pass.
- **MIR `lower_place_expr` resolved nested fields against
  the wrong struct.** `o.inner.x = 100` looked up `x` on the
  outer struct, didn't find it, and silently dropped the
  assign. Now prefers `struct_name_from_expr(receiver)` (the
  projected type), with `local_struct[base.local]` as
  fallback.
- **`..base` functional-update was discarded.** HIR
  `lower_struct_literal` ignored the `base` field. Added a
  synthetic `__base` key that carries the base expression;
  MIR projects `base.field` for every unprovided field; VM
  `builtin_struct_new` strips the synthetic key.
- **Closure free-var capture pulled in synthetic helpers.**
  `__concat`, `__struct`, `__fmt_prec`, `format!`, etc. were
  walked as free variables and captured into closure envs.
  `walk_free` now excludes them.
- **`FnTrait`-typed locals weren't recognised as indirect
  callees.** Closure-returning-closure fell through to
  direct-name dispatch. MIR's callee-kind match now accepts
  `TyKind::FnTrait(_)` and routes through `Operand::Copy`.
- **`errors::Error` printed as a struct literal.** Now
  renders as the `message` field via the `Display` impl.
- **`os::exit(N)` swallowed buffered stdout.** Calls
  `gos_rt_flush_stdout` before `process::exit`.
- **`i128` / `u128` silently truncated to i64 in compiled
  tiers.** Now bails with a "compiled tier" diagnostic the
  test gate matches.
- **User-defined `pub fn substring(s, ...)` recursed via
  method dispatch.** `s.substring(a, b)` is now resolved
  to the runtime `String::substring` helper before falling
  through to user dispatch - restores `String::method` to
  the qualified-method dispatch keys.
- **Stray `feature-testing-examples/project.toml` was
  forcing all 52 examples into every build.** The CLI's
  sibling-bundle walked one parent up; the stray manifest
  made it bundle examples too. Removed the stray file.
- **Multi-file sibling-module regression fixed.** Cross-module
  function calls (`mod foo;` + `foo::bar()` from sibling
  `src/*.gos` files) compiled clean under `gos check` but failed
  at runtime with `error[GX0002]: name 'foo::bar' is not bound in
  this scope`. Root cause was layered: the CLI driver didn't
  auto-bundle siblings; the resolver only registered `mod` heads
  (no recursion into nested items); the type checker /
  exhaustiveness walkers stopped at module boundaries; the parser
  silently dropped `use` decls inside inline `mod` bodies; HIR
  carried no module-path so the interp / VM globals lost the
  qualified spelling; and the Cranelift JIT was missing
  `gos_rt_eprint_str` / `gos_rt_eprintln`. Each layer was
  patched. New regression test
  `cross_file_chained_sibling_module_calls` covers a 3-deep call
  chain across all three execution tiers.
- `String::as_bytes()` was registered as a runtime global but
  silently mis-wrote the byte slice through `os::write_file`.
  The method is now rejected at `gos check` time
  (`GT0002: no method named 'as_bytes' found for type 'String'`)
  via a new `KNOWN_METHOD_NAMES` allow-list in the type checker.
  Pass `&String` directly to byte-consuming APIs; the runtime
  binding is gone.
- `encoding::json` parser cast UTF-8 bytes through `char` as
  Latin-1, mangling all non-ASCII text. `\uXXXX` escapes weren't
  handled either. Now reads bytes properly and decodes
  `\uXXXX` (including surrogate pairs). The previously-broken
  `unicode_strings_preserve_through_round_trip` test now passes.
- Aggregate construction is now heap-allocated (`calloc`) instead
  of stack-slot. Returning a struct from a method (e.g.
  `Celsius { value: ... }.to_fahrenheit()`) no longer aliases the
  next call's stack slot; `temperature.gos` now matches across
  tiers.
- `loop { ... break <expr> }` captures the break expression's
  value in compiled mode. Previously
  `let x = loop { ... break sq }` returned 0 instead of `sq`.
- `result.map_err(closure)` and `result.map(closure)` dispatch
  correctly when the receiver type is unresolved at HIR time
  (e.g. `text.parse().map_err(...)?`). The closure was being
  built and silently dropped.
- String equality (`s == "literal"`, `s != "literal"`) routes
  through `gos_rt_str_eq`. Previously a pointer-compare that
  silently disagreed with interpreted output whenever the string
  came from a runtime helper rather than a literal-pinned slot.
- Reference deref (`*p` where `p: &i64` / `&f64` / `&bool` /
  `&char`) emits a real load instead of returning the pointer
  unchanged. Affected every iterator pattern that yields scalar
  references.
- `s.as_bytes()` returns a `Vec<i64>` shape (one zero-extended
  byte per slot) instead of a packed `Vec<u8>`. Compiled `bytes[i]`
  indexing now reads the byte's value through
  `gos_rt_vec_get_i64` rather than reading 8 packed buffer
  bytes as a single i64 (`reverse_string.gos` reproducer).
- `<chain>.method().to_string()` dispatches to the right runtime
  formatter (`gos_rt_i64_to_str` / `gos_rt_f64_to_str` /
  identity for strings) when the typechecker leaves the chain's
  HIR type as a `Var(_)`. Previously the identity-copy fallback
  fed an i64 to `gos_rt_str_concat` as a c_char* - segfault.
- Better error messages.
- Actual Error types.

### Added

- **`std::http` client now covers GET, POST, PUT, OPTIONS,
  DELETE, HEAD plus `http::request(method, url, body, headers)`
  for arbitrary methods.** ureq + rustls under the hood; HTTPS
  via Mozilla roots. Free-function wrappers (`http::get`,
  `http::post`, ...) and method-style on `Client` (`Client::post`,
  `Client::put`, ...) both round-trip through one
  `do_request(method, url, body, headers)` core. Unknown method
  strings return `Err(transport)`.
- **`http::stream(method, url, body, headers) -> ResponseStream`**
  for SSE / chunked bodies. `ResponseStream::next_line()` reads
  one line at a time from a `BufReader<Box<dyn Read + Send +
  Sync>>` over `ureq::Response::into_reader()`. Stream handles
  live in a process-wide registry keyed by i64 so they survive
  across `next_line()` calls. No temp files, no shell-out --
  this replaces the curl-and-poll pattern users were forced
  into for streaming.
- **Stdlib surface filled in.** `std::os` gained `cwd`,
  `set_cwd`, `set_env`, `unset_env`, `is_file`, `is_dir`,
  `is_symlink`, `file_size`, `remove_dir`, `remove_dir_all`,
  `copy`, `canonicalize`, `home`, `temp_dir`. `std::fs::metadata`
  returns a real `Metadata` struct. `std::net` exposes
  `TcpListener::{bind, accept, local_addr, close}`,
  `TcpStream::{connect, read, read_to_string, write, write_all,
  close}`, `UdpSocket::{bind, send_to, recv_from, local_addr,
  close}`, `net::resolve` / `net::lookup`. `std::sync` exposes
  `AtomicI64::{new, load, store, fetch_add, fetch_sub,
  compare_and_swap}`, `AtomicBool::{new, load, store,
  compare_and_swap}`, `Mutex::{new, lock, store}`, `Once::{new,
  call}`. `std::strings` adds `join`, `trim_start`, `trim_end`,
  `strip_prefix`, `strip_suffix`, `pad_left`, `pad_right`,
  `rfind`, `replacen`. `std::strconv` adds `parse_int`,
  `parse_i64`, `parse_u64`, `parse_float`, `parse_f64`,
  `parse_bool`, `format_int`, `format_i64`, `format_float`,
  `format_f64`, `itoa`/`atoi`. `std::time` adds `Instant::{now,
  elapsed_ms}`, `Duration::{from_millis, from_secs, from_micros,
  as_millis, as_secs, as_micros}`, `time::now_nanos`,
  `monotonic_ms`, `monotonic_nanos`, `since_ms`. `std::path`
  adds `parent`, `file_name`, `stem`, `ext`, `is_absolute`,
  `normalize`. `std::utf8` adds `count_runes`, `rune_len`,
  `is_valid`. `std::bufio` adds `read_to_string`,
  `read_lines_of`, `split_whitespace`.
  `std::collections::HashSet` was a HashMap stub; now a real set
  with `insert`, `remove`, `contains`, `len`, `is_empty`,
  `clear`, `to_vec`, `iter`.
- **Type-checker shifted left.** New `KNOWN_METHOD_NAMES` gate
  in `gossamer-types/src/checker.rs` rejects calls to method
  names that aren't bound at runtime (catches `as_bytes`,
  `and_then`, `filter`, `collect`, etc.) at `gos check` time
  with `GT0002` instead of letting them through to a runtime
  panic. User-defined `impl` methods are tracked separately so
  they're never falsely flagged.

### Performance

- Bytecode VM method-call IC hit path now takes a shared
  `RefCell::borrow()` instead of `borrow_mut()`. The cache is
  read-only on hit; the previous `borrow_mut()` serialised every
  call against any other borrow on the same RefCell.
- JIT tier-up threshold scales by chunk instruction count
  (`HOT_THRESHOLD_BASE * 50 / max(50, instr_count)`, floored at
  `HOT_THRESHOLD_FLOOR = 16`). Big functions now tier up after a
  handful of entries instead of waiting for 100 full calls of an
  expensive body. Honoured by `GOSSAMER_JIT_THRESHOLD` env var.
- Bytecode VM now decrements the JIT hot counter on backward
  `Op::Jump` and on the new fused `IncJumpIfLt/LeI64` ops.
  Loop-shaped chunks reachable only through their own internal
  control flow (rather than via repeated call entries) tier up
  on the same path.
- Cranelift `gos_rt_vec_len` is inlined as a null-check + offset-0
  load (matches the GosVec `repr(C)` layout). For-loop bounds and
  every `vec.len()` access in compiled code skip the C-ABI call.
- Per-thread shadow-stack root tracking is lock-free on the hot
  path. Owner reads/writes a 1024-slot `Box<[AtomicU32]>` with
  Relaxed stores and a Release-published `len`; the cross-thread
  mark snapshot Acquire-loads `len` and walks slots without
  taking any lock. Spillover into a `Mutex<Vec<u32>>` only when
  call depth exceeds the in-array capacity. The earlier design
  paid an uncontended `parking_lot::Mutex` lock+unlock at every
  function prologue and epilogue.
- For-range `for i in a..b { ... }` lowers to a header
  bounds-check + body + fused `IncJumpIfLt/LeI64` op that
  combines the per-iter `AddI64 + Jump` into one dispatch.
- `format!`, `panic!`, `eprintln!`, `eprint!` now build their
  message through the runtime's batched concat buffer
  (`gos_rt_concat_init` / `_str` / `_i64` / `_f64` / `_bool` /
  `_char` / `_finish`) instead of chaining N-1 pairwise
  `gos_rt_str_concat` calls. Eliminates the throwaway
  intermediate strings that the serial chain allocated and
  dropped between each pair of args.
- JIT trampoline `MAX_ARGS` raised from 8 to 12 (homogeneous
  i64-only and f64-only shapes for arities 9-12). HTTP handlers,
  multi-arg `format!` callees, and other 8+-arg helpers no
  longer fall back to bytecode purely because of arity.

### Stdlib parity

- `flag` stdlib fully wired in compiled mode. Default values for
  `int`, `float`, `duration`, `string_list`, `short`, `usage` are now
  honoured (previously every non-`string`/`uint`/`bool` flag silently
  zeroed). `parse` accepts the `=` form, short aliases, `--`, and
  `--help` / `-h`. Interp gained matching `float` / `duration` /
  `string_list` / `usage` builtins so both tiers produce identical
  output across every flag method.
- `flag::define(name, [flag::int(...), flag::string(...),
  flag::bool(...)])` (declarative one-shot constructor) now lowers
  to the imperative `flag::Set` builder chain at MIR time.
  Previously interp-only - compiled mode silently returned a
  null-shaped struct so `*flags.<long>` always yielded the
  primitive zero.
- `os::env`, `os::cwd` wired in both tiers. Compiled mode was
  returning `0` for every env var lookup and `0` for `cwd`.
- `fs::list_dir` wired in compiled mode (returns
  `Result<[DirInfo], Error>`).
- `time::Duration::from_secs` / `from_millis` lower in compiled mode.

### Test coverage

- `cargo test -p gossamer-cli --test parity --features
  exhaustive_tests --release` walks every example in
  `examples/*.gos` under both tiers and asserts byte-identical
  stdout/stderr/exit code. Two examples (`go_spawn.gos`,
  `list_dir.gos`) are listed in `KNOWN_DIVERGENT_EXAMPLES` with
  explicit root-cause comments - go_spawn requires a
  deterministic scheduler shared between tiers, list_dir
  requires registering `fs::DirInfo` as a stdlib struct in
  `gossamer-types::TyCtxt::register_struct_fields` at
  typechecker startup. Every other example round-trips.
- `crates/gossamer-codegen-cranelift/tests/correct/p51_flag_defaults`
  walks every flag type through interp + Cranelift + LLVM tiers.

## 0.0.1

### Stdlib parity

- `flag` stdlib fully wired in compiled mode. Default values for
  `int`, `float`, `duration`, `string_list`, `short`, `usage` are now
  honoured (previously every non-`string`/`uint`/`bool` flag silently
  zeroed). `parse` accepts the `=` form, short aliases, `--`, and
  `--help` / `-h`. Interp gained matching `float` / `duration` /
  `string_list` / `usage` builtins so both tiers produce identical
  output across every flag method.
- `flag::define(name, [flag::int(...), flag::string(...),
  flag::bool(...)])` (declarative one-shot constructor) now lowers
  to the imperative `flag::Set` builder chain at MIR time.
  Previously interp-only - compiled mode silently returned a
  null-shaped struct so `*flags.<long>` always yielded the
  primitive zero.
- `os::env`, `os::cwd` wired in both tiers. Compiled mode was
  returning `0` for every env var lookup and `0` for `cwd`.
- `fs::list_dir` wired in compiled mode (returns
  `Result<[DirInfo], Error>`).
- `time::Duration::from_secs` / `from_millis` lower in compiled mode.

### Compiler / codegen fixes

- Aggregate construction is now heap-allocated (`calloc`) instead
  of stack-slot. Returning a struct from a method (e.g.
  `Celsius { value: ... }.to_fahrenheit()`) no longer aliases the
  next call's stack slot; `temperature.gos` now matches across
  tiers.
- `loop { ... break <expr> }` captures the break expression's
  value in compiled mode. Previously
  `let x = loop { ... break sq }` returned 0 instead of `sq`.
- `result.map_err(closure)` and `result.map(closure)` dispatch
  correctly when the receiver type is unresolved at HIR time
  (e.g. `text.parse().map_err(...)?`). The closure was being
  built and silently dropped.
- String equality (`s == "literal"`, `s != "literal"`) routes
  through `gos_rt_str_eq`. Previously a pointer-compare that
  silently disagreed with interpreted output whenever the string
  came from a runtime helper rather than a literal-pinned slot.
- Reference deref (`*p` where `p: &i64` / `&f64` / `&bool` /
  `&char`) emits a real load instead of returning the pointer
  unchanged. Affected every iterator pattern that yields scalar
  references.
- `s.as_bytes()` returns a `Vec<i64>` shape (one zero-extended
  byte per slot) instead of a packed `Vec<u8>`. Compiled `bytes[i]`
  indexing now reads the byte's value through
  `gos_rt_vec_get_i64` rather than reading 8 packed buffer
  bytes as a single i64 (`reverse_string.gos` reproducer).
- `<chain>.method().to_string()` dispatches to the right runtime
  formatter (`gos_rt_i64_to_str` / `gos_rt_f64_to_str` /
  identity for strings) when the typechecker leaves the chain's
  HIR type as a `Var(_)`. Previously the identity-copy fallback
  fed an i64 to `gos_rt_str_concat` as a c_char* - segfault.
- Better error messages.
- Actual Error types.

### Test coverage

- `cargo test -p gossamer-cli --test parity --features
  exhaustive_tests --release` walks every example in
  `examples/*.gos` under both tiers and asserts byte-identical
  stdout/stderr/exit code. Two examples (`go_spawn.gos`,
  `list_dir.gos`) are listed in `KNOWN_DIVERGENT_EXAMPLES` with
  explicit root-cause comments - go_spawn requires a
  deterministic scheduler shared between tiers, list_dir
  requires registering `fs::DirInfo` as a stdlib struct in
  `gossamer-types::TyCtxt::register_struct_fields` at
  typechecker startup. Every other example round-trips.
- `crates/gossamer-codegen-cranelift/tests/correct/p51_flag_defaults`
  walks every flag type through interp + Cranelift + LLVM tiers.

## 0.0.0

Initial release. Not production ready.
