// A Tour of Gossamer - lesson data plus the page controller.
//
// Renders the step list, the current lesson (prose on the left, a live
// "Try Gossamer" playground on the right), and the prev/next controls
// into #tour-root, and keeps the visible lesson in sync with
// location.hash so every step is deep-linkable and the browser's
// back/forward buttons work without extra wiring.
//
// The playground module is imported lazily so the tour chrome paints
// immediately and a load failure degrades to a readable code listing
// instead of a blank panel.

const PLAYGROUND_URL = "../playground/playground.js";
const PLAY_HEIGHT = "320px";

const LESSONS = [
  {
    slug: "hello",
    title: "Hello + values",
    prose: `
      <p>Gossamer is a fast-compiling language with a Rust-flavoured
      surface and a Go-shaped runtime. Bindings are <strong>immutable by
      default</strong> - reach for <code>let mut</code> only when a value
      genuinely changes after construction.</p>
      <p>String literals are already <code>String</code>, so there is no
      <code>.to_string()</code> noise. <code>println!</code> pulls bindings
      straight from scope by name - <code>{name}</code> - and the
      format macros are built in: there are no user-defined macros.</p>
      <p>Press <strong>Run</strong> (or Ctrl / Cmd + Enter) to execute the
      program on the right. Edit it freely and run it again.</p>`,
    code: `// Bindings are immutable by default; reach for \`let mut\` only when
// a value really changes. String literals are already \`String\`.
let name = "Gossamer"
let pi = 3.14159

let greeting = "hello, " + &name
println!("{greeting}!")

// Named interpolation reads bindings straight from scope.
println!("{name} is {} bytes long", name.len())
println!("pi is about {pi}")
`,
  },
  {
    slug: "pipes",
    title: "Forward pipe |>",
    prose: `
      <p>The forward-pipe <code>|></code> turns nested calls into
      left-to-right dataflow. <code>x |> f</code> is <code>f(x)</code>;
      <code>x |> f(a)</code> threads the value into the <strong>last</strong>
      argument, so it reads <code>f(a, x)</code>.</p>
      <p>The <code>_</code> placeholder instead makes the piped value the
      receiver: <code>x |> _.trim</code> is <code>x.trim()</code>. And
      ranges are plain values, so a combinator chain like
      <code>(1..=5).filter(...).sum()</code> needs no pipe at all - pick
      whichever direction reads best.</p>`,
    code: `fn double(x: i64) -> i64 { x * 2 }
fn add(a: i64, b: i64) -> i64 { a + b }

// \`x |> f\` is \`f(x)\`; \`x |> f(a)\` lands x in the last slot: \`f(a, x)\`.
let n = 3 |> double |> add(10)
println!("3 |> double |> add(10) = {n}")

// \`_.method\` pipes a value through its own methods - \`_\` is the receiver.
let shout = "  hi there  " |> _.trim |> _.to_uppercase
println!("shout = {shout}")

// Ranges are values and combinators are methods - chain them directly.
let total = (1..=5).filter(|n| n % 2 == 1).sum()
println!("sum of odds in 1..=5 = {total}")
`,
  },
  {
    slug: "closures",
    title: "Closures + higher-order fns",
    prose: `
      <p>A closure is <code>|param: T| body</code> and captures its
      environment automatically - there is no <code>move</code>. Two
      callable types name a function value: <code>fn(args) -> ret</code>
      for a bare code pointer, and <code>Fn(args) -> ret</code> - the
      callable trait - which also accepts capturing closures.</p>
      <p>A higher-order function takes an <code>Fn(...)</code> parameter,
      and a bare <code>fn</code> coerces into it at the call site. The
      sequence combinators - <code>filter</code>, <code>map</code>,
      <code>sum</code> - are higher-order all the way down, taking a
      closure per step.</p>`,
    code: `// \`Fn(i64) -> i64\` accepts both capturing closures and bare functions.
fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }

fn inc(y: i64) -> i64 { y + 1 }

fn main() {
    let scale = 10
    let scaled = |y: i64| scale * y       // captures \`scale\`
    println!("scaled(5) = {}", apply(scaled, 5))
    println!("inc(41)   = {}", apply(inc, 41))   // bare fn coerces

    // Closures power the sequence combinators, one per step.
    let total = (1..=6).filter(|n| n % 2 == 0).map(|n| n * n).sum()
    println!("sum of squares of evens in 1..=6 = {total}")
}
`,
  },
  {
    slug: "match",
    title: "Pattern matching + enums",
    prose: `
      <p>Enums are sum types - a variant can carry a tuple payload, named
      fields, or nothing at all. <code>match</code> is an
      <strong>expression</strong> and must be <strong>exhaustive</strong>:
      the compiler rejects a missing case, so there is no silent
      fallthrough.</p>
      <p>Patterns bind payloads directly, with no dereference, and arms can
      use ranges, guards, and tuple shapes. Here each <code>Shape</code>
      variant maps to its own area formula.</p>`,
    code: `enum Shape {
    Circle(f64),
    Rect { w: f64, h: f64 },
    Line,
}

// \`match\` is exhaustive and yields a value - every variant is handled.
fn area(shape: &Shape) -> f64 {
    match shape {
        Shape::Circle(r) => 3.14159 * *r * *r,
        Shape::Rect { w, h } => *w * *h,
        Shape::Line => 0.0,
    }
}

let shapes = [Shape::Circle(2.0), Shape::Rect { w: 3.0, h: 4.0 }, Shape::Line]
for s in shapes {
    println!("area = {}", area(s))
}
`,
  },
  {
    slug: "recursive-enum",
    title: "Recursive enums",
    prose: `
      <p>An enum variant can refer to the enum itself, so a tree or an
      expression type needs nothing special: a variant payload can be the
      enum, and every node is heap-shared automatically. A
      <code>match</code> reaches straight through with no dereference.</p>
      <p><code>Box</code>, <code>Arc</code>, and <code>Rc</code> are
      transparent - you can write <code>Box&lt;Expr&gt;</code> to signal
      heap sharing, but the bare <code>Expr</code> form compiles to the
      same thing and is what most code uses. Here a tiny arithmetic
      evaluator recurses with one exhaustive <code>match</code> - the
      whole shape of a real tree-walking interpreter.</p>`,
    code: `// A recursive enum needs no wrapping: a variant payload can be the enum
// itself, and every node is heap-shared. \`Box\`/\`Arc\`/\`Rc\` are
// transparent, so \`Box<Expr>\` would compile to exactly the same thing.
enum Expr {
    Num(i64),
    Add(Expr, Expr),
    Mul(Expr, Expr),
}

fn eval(e: &Expr) -> i64 {
    match e {
        Expr::Num(n) => *n,
        Expr::Add(a, b) => eval(a) + eval(b),
        Expr::Mul(a, b) => eval(a) * eval(b),
    }
}

fn main() {
    // (2 + 3) * 4
    let tree = Expr::Mul(Expr::Add(Expr::Num(2), Expr::Num(3)), Expr::Num(4))
    println!("(2 + 3) * 4 = {}", eval(&tree))
}
`,
  },
  {
    slug: "options",
    title: "Option + the ? operator",
    prose: `
      <p><code>Option&lt;T&gt;</code> is <code>Some(v)</code> or
      <code>None</code> - the type-level "maybe absent", with no null in
      sight. Read it with <code>if let Some(v) = ...</code>, and inside
      an <code>Option</code>-returning function <code>?</code>
      short-circuits the moment a value is <code>None</code>.</p>
      <p>It is the same <code>?</code> that propagates <code>Err</code>
      in a <code>Result</code> function: the present path stays flat
      while the absent path needs no nesting at all.</p>`,
    code: `// \`?\` propagates \`None\` inside an Option-returning function.
fn first_even(xs: &[i64]) -> Option<i64> {
    for x in xs {
        if *x % 2 == 0 { return Some(*x) }
    }
    None
}

// \`?\` chains Option-returning calls: if \`first_even\` yields \`None\`,
// this returns \`None\` immediately - no nesting on the absent path.
fn half_of_first_even(xs: &[i64]) -> Option<i64> {
    let n = first_even(xs)?
    Some(n / 2)
}

fn main() {
    if let Some(n) = first_even(&[3, 5, 8, 9]) {
        println!("first even = {n}")
    }

    println!("half = {:?}", half_of_first_even(&[3, 5, 8, 9]))
    println!("half = {:?}", half_of_first_even(&[1, 3, 5]))
}
`,
  },
  {
    slug: "errors",
    title: "Error handling",
    prose: `
      <p>Gossamer has no exceptions. Fallible functions return
      <code>Result&lt;T, E&gt;</code> and <code>?</code> propagates the
      <code>Err</code> branch upward. Build and chain typed errors with
      <code>std::errors</code>: <code>errors::new</code>,
      <code>errors::wrap</code> for higher-level context, and printing a
      wrapped error shows the colon-joined cause chain.</p>
      <p>Handle the present and error paths with a small <code>match</code>
      when the recovery action is side-effecting. Use the data-last
      <code>std::result</code> helpers when you are transforming values
      inside a pipeline.</p>`,
    code: `use std::errors

// Fallible work returns \`Result<T, E>\`; \`?\` propagates the \`Err\`.
fn parse_port(text: &String) -> Result<i64, errors::Error> {
    let n: i64 = match text.parse() {
        Ok(n) => n,
        Err(_) => return Err(errors::new(format!("not a number: {text}"))),
    }
    if n <= 0 { return Err(errors::new(format!("must be positive: {n}"))) }
    Ok(n)
}

fn main() {
    // Match the Ok value; handle the Err in-line.
    match parse_port(&"8080") {
        Ok(n) => println!("port = {n}"),
        Err(e) => eprintln!("error: {}", e.message()),
    }

    // \`wrap\` adds context; printing shows the colon-joined cause chain.
    let bad = parse_port(&"oops").map_err(|e| errors::wrap(e, "loading config"))
    if let Err(e) = bad { println!("{e}") }
}
`,
  },
  {
    slug: "collections",
    title: "Vec / HashMap / iterators",
    prose: `
      <p>The built-in growable array is <code>[T]</code> - push, index, and
      iterate with <code>for x in xs</code> (no <code>.iter()</code>, no
      <code>as usize</code> on indices). Richer containers live in
      <code>std::collections</code>: <code>HashMap</code>,
      <code>HashSet</code>, and <code>BTreeMap</code>.</p>
      <p><code>HashMap</code> carries ergonomic helpers - <code>m.inc(k)</code>
      for counters and <code>m.get_or(k, default)</code> for a fallback read.
      Combinators such as <code>filter</code> and <code>sum</code> are
      methods on any Vec or range, so a query never needs a manual
      loop.</p>`,
    code: `fn main() {
    // A growable Vec; iterate the values directly.
    let mut nums = [4, 8, 15, 16, 23]
    nums.push(42)
    println!("count = {}, last = {}", nums.len(), nums.last().unwrap_or(0))

    // Combinators are methods on any Vec or range.
    println!("sum of evens = {}", nums.filter(|n| n % 2 == 0).sum())

    // HashMap counters: \`inc\` does the get-or-zero-then-add for you.
    let mut tally = {}
    for word in ["go", "go", "rust", "go"] {
        tally.inc(word)
    }
    println!("go appears {} times", tally.get_or("go", 0))
}
`,
  },
  {
    slug: "strings",
    title: "Strings + format! specs",
    prose: `
      <p>Strings are values, not references you juggle. Concatenate with
      <code>+</code>, append with <code>+=</code>, and pipe a string through
      its own methods with <code>_.method</code> -
      <code>title |> _.trim |> _.to_title</code>.</p>
      <p>Formatted output follows Rust's <code>{:spec}</code> grammar: width
      and alignment (<code>{:>8}</code>, <code>{:^7}</code>), zero-padding and
      radix (<code>{:08x}</code>), and precision (<code>{:.2}</code>), for
      both positional and named arguments.</p>`,
    code: `fn main() {
    let title = "  the gossamer tour  "

    // Method chaining through \`_.method\`; strings are plain values.
    let clean = title |> _.trim |> _.to_title
    println!("[{clean}]")

    // \`+=\` appends; \`+\` concatenates with no separator.
    let mut line = "items:"
    for part in ["alpha", "beta", "gamma"] {
        line += " " + &part
    }
    println!("{line}")

    // Format specs follow Rust's {:spec} grammar.
    println!("[{:>8}]", 42)
    println!("[{:08x}]", 255)
    println!("[{:^7}]", "hi")
    println!("[{:.2}]", 3.14159)
}
`,
  },
  {
    slug: "json",
    title: "JSON round-trip",
    prose: `
      <p>Every user <code>struct</code> automatically gains two generic
      free functions: <code>to_json::&lt;T&gt;(&amp;value)</code> encodes
      it to text and <code>from_json::&lt;T&gt;(&amp;text)</code> decodes
      text back into a typed value, validating each field against its
      declared type with path-qualified errors.</p>
      <p>There is one spelling - the turbofish form - and it works on
      every tier. Here a <code>Server</code> config makes the full round
      trip; for unknown-shape documents the dynamic
      <code>json::parse</code> API stays available.</p>`,
    code: `// Every user struct gets \`to_json::<T>\` / \`from_json::<T>\` for free.
// \`?\` makes this entry file's implicit \`main\` return a Result.
#[derive(Debug)]
struct Server { host: String, port: i64, tls: bool }

let cfg = Server { host: "localhost", port: 8080, tls: true }

// Encode the struct to JSON text...
let text = to_json::<Server>(&cfg)?
println!("encoded = {text}")

// ...then decode it straight back into a typed struct, validating
// each field against its declared type.
let back: Server = from_json::<Server>(&text)?
println!("decoded = {:?}", back)
println!("address = {}:{}", back.host, back.port)
`,
  },
  {
    slug: "regex",
    title: "Regular expressions",
    prose: `
      <p><code>std::regex</code> wraps Rust's <code>regex</code> crate.
      <code>compile</code> once into a <code>Pattern</code> - it carries
      its source for diagnostics - then reuse it across
      <code>is_match</code>, <code>find</code> / <code>find_all</code>,
      <code>captures</code>, <code>replace_all</code>, and
      <code>split</code>.</p>
      <p><code>captures</code> returns <code>[full, group1, ...]</code>,
      each an <code>Option&lt;String&gt;</code>. A
      <strong>let-chain</strong> binds every group and tests them in one
      condition, so a structured parse collapses to a single
      <code>if</code>.</p>`,
    code: `use std::regex

fn main() {
    // Compile once; the pattern carries its source for diagnostics.
    let re = match regex::compile("([0-9]{4}-[0-9]{2}-[0-9]{2}) ([A-Z]+) (.+)") {
        Ok(r) => r,
        Err(e) => { eprintln!("bad pattern: {e}"); return }
    }

    let lines = ["2026-06-29 ERROR disk full", "2026-06-30 INFO restarted"]
    for line in lines {
        // \`captures\` yields [full, group1, group2, ...]; a let-chain
        // binds all three groups and tests them in one condition.
        if let Some(c) = regex::captures(&re, &line)
            && let Some(date) = c[1]
            && let Some(level) = c[2]
            && let Some(msg) = c[3] {
            println!("{date}  [{level}]  {msg}")
        }
    }
}
`,
  },
  {
    slug: "goroutines",
    title: "Goroutines + channels",
    prose: `
      <p>Concurrency is Go-shaped: <code>go expr</code> spawns a goroutine and
      <code>channel()</code> returns a typed <code>(Sender, Receiver)</code>
      pair. The producer sends every value and <code>close()</code>s the
      channel; the consumer drains it with the canonical
      <code>while let Some(v) = rx.recv()</code>.</p>
      <p>Here the producer runs to completion before the drain finishes,
      which is the subset the in-browser runtime supports.
      <strong>Full goroutine interleaving</strong> - goroutines that block
      and hand off mid-run - runs natively with <code>gos</code>.</p>`,
    code: `use std::sync

// The producer sends every value, then closes - it runs to
// completion, so no mid-run hand-off is needed.
fn produce(tx: sync::Sender<i64>) {
    for n in 1..=5 { tx.send(n * n) }
    tx.close()
}

fn main() {
    let (tx, rx) = sync::channel(5)
    go produce(tx)

    // \`recv\` yields \`Some\` until the channel is closed and drained.
    let mut total = 0
    while let Some(v) = rx.recv() { total += v }
    println!("sum of squares 1..=5 = {total}")
}
`,
  },
  {
    slug: "select",
    title: "select over channels",
    prose: `
      <p><code>select</code> multiplexes several channel operations at
      once: each arm is a receive or a send, the arms are polled in
      source order, and the goroutine takes the first one that is ready.
      An optional <code>default</code> arm fires when none is.</p>
      <p>Here two producers feed two channels and <code>select</code>
      merges their values as they arrive - the standard fan-in pattern.
      Full mid-run hand-off between goroutines that block and resume runs
      natively with <code>gos</code>.</p>`,
    code: `use std::sync

fn produce(tx: sync::Sender<i64>, xs: Vec<i64>) {
    for x in xs { tx.send(x) }
}

fn main() {
    let (tx_hi, rx_hi) = sync::channel(3)
    let (tx_lo, rx_lo) = sync::channel(2)

    go produce(tx_hi, [1, 2, 3])
    go produce(tx_lo, [10, 20])

    // Five values arrive across two channels; \`select\` takes whichever
    // arm is ready, polling them in source order.
    let mut total = 0
    for _ in 0..5 {
        select {
            v = rx_hi.recv() => total += v,
            v = rx_lo.recv() => total += v,
        }
    }
    println!("merged total = {total}")
}
`,
  },
  {
    slug: "shared-state",
    title: "Shared state + WaitGroup",
    prose: `
      <p>When sharing memory is simpler than passing messages, reach for
      the <code>std::sync</code> primitives. A <code>WaitGroup</code>
      tracks outstanding work: <code>add</code> before spawning,
      <code>done</code> as each worker finishes, and <code>wait</code>
      blocks until the count reaches zero - no sleeps.</p>
      <p>The shared counter here is an <code>AtomicI64</code>, so
      concurrent <code>fetch_add</code>s compose without an explicit
      lock. Sync handles are shared by value: a copy refers to the same
      underlying state.</p>`,
    code: `use std::sync

// Each worker runs to completion: it folds its share into the shared
// atomic counter, then signals the WaitGroup. Sync handles are shared
// by value - a copy refers to the same underlying state.
fn worker(total: sync::AtomicI64, wg: sync::WaitGroup, i: i64) {
    sync::AtomicI64::fetch_add(total, i * i)
    wg.done()
}

fn main() {
    let total = sync::AtomicI64::new(0)
    let wg = sync::WaitGroup::new()

    for i in 1..=4 {
        wg.add(1)
        go worker(total, wg, i)
    }
    wg.wait()      // block until every worker has called \`done\`

    println!("sum of squares 1..=4 = {}", sync::AtomicI64::load(total))
}
`,
  },
  {
    slug: "types",
    title: "Structs / traits / generics / derive",
    prose: `
      <p>Structs are runtime-managed value types; traits define a shared
      interface that each type implements. They compare by value and
      <code>.clone()</code> with no derive, so <code>==</code> and
      <code>.clone()</code> just work on every tier;
      <code>#[derive(Debug)]</code> adds the <code>{:?}</code>
      representation (<code>Default</code>, <code>PartialOrd</code>, and
      <code>Ord</code> are derivable too).</p>
      <p>Generic functions take trait bounds -
      <code>fn farther&lt;T: Distance&gt;(...)</code> - and each call site
      monomorphises to a direct call, with no dynamic dispatch.</p>`,
    code: `// Structs compare by value and \`.clone()\` with no derive; derive Debug for {:?}.
#[derive(Debug)]
struct Point { x: i64, y: i64 }

trait Distance {
    fn from_origin(&self) -> i64
}

impl Distance for Point {
    fn from_origin(&self) -> i64 { self.x * self.x + self.y * self.y }
}

// Generic over any \`Distance\`, dispatched statically and monomorphised.
fn farther<T: Distance>(a: &T, b: &T) -> bool {
    a.from_origin() > b.from_origin()
}

fn main() {
    let a = Point { x: 3, y: 4 }
    let b = Point { x: 1, y: 2 }
    println!("a = {:?}", a)
    println!("a == a.clone(): {}", a == a.clone())
    println!("a farther than b: {}", farther(&a, &b))
}
`,
  },
  {
    slug: "operators",
    title: "Operator overloading",
    prose: `
      <p>Arithmetic operators are trait methods, so a type opts into them
      by implementing the trait: <code>impl Add for Vec2</code> gives
      <code>Vec2 + Vec2</code> its meaning, and the same shape covers
      <code>Sub</code>, <code>Mul</code>, and the rest.</p>
      <p>These are written by hand, not derived. The compiler already
      synthesizes <code>==</code>, ordering, and <code>.clone()</code>
      for you - a custom <code>+</code> is the part that is genuinely
      yours to define.</p>`,
    code: `// Operator overloading: \`impl Add for T\` makes \`+\` work on your type.
struct Vec2 { x: f64, y: f64 }

impl Add for Vec2 {
    fn add(self, other: Vec2) -> Vec2 {
        Vec2 { x: self.x + other.x, y: self.y + other.y }
    }
}

fn main() {
    let a = Vec2 { x: 1.0, y: 2.0 }
    let b = Vec2 { x: 3.0, y: 4.0 }
    let c = a + b
    println!("sum = ({}, {})", c.x, c.y)
}
`,
  },
  {
    slug: "comptime",
    title: "Compile-time evaluation",
    prose: `
      <p>A <code>comptime fn</code> runs during compilation. Inside a
      <code>comptime { }</code> block its result is folded to a literal
      before the program runs, so the work happens once, at build time,
      and every tier embeds the same constant.</p>
      <p>Use it for lookup tables and derived constants - anything better
      computed at build time than on every run.
      <code>const FACT_10: i64 = comptime { factorial(10) }</code> ships
      the answer, not the loop that produced it.</p>`,
    code: `// A \`comptime fn\` runs at compile time; calling it inside a
// \`comptime { }\` block folds the result into a literal, so the runtime
// never repeats the work - every tier compiles the same constant.
comptime fn factorial(n: i64) -> i64 {
    let mut acc = 1
    for i in 2..=n { acc *= i }
    acc
}

const FACT_10: i64 = comptime { factorial(10) }

fn main() {
    println!("10! folded at compile time = {FACT_10}")

    // A \`comptime\` block can fold an inline computation to a literal too.
    let triangular = comptime {
        let mut acc = 0
        for i in 1..=100 { acc += i }
        acc
    }
    println!("sum 1..=100 = {triangular}")
}
`,
  },
  {
    slug: "arena",
    title: "arena { } regions",
    prose: `
      <p>Memory is automatic: deterministic reference counting, no borrow
      checker, no GC pauses. For an object graph that all dies together,
      an <code>arena { }</code> block bump-allocates everything inside it
      and frees the whole region at every exit path.</p>
      <p>The contract is simple: nothing allocated inside may escape, so
      you compute a scalar or string summary inside and keep that. Here
      each round fills a throwaway buffer and only the running total
      survives the block.</p>`,
    code: `fn main() {
    // Everything allocated inside an \`arena { }\` is bump-allocated and
    // freed wholesale at every exit. The contract: nothing allocated
    // inside may escape, so compute a scalar summary and keep that.
    let mut grand_total = 0
    for round in 1..=3 {
        arena {
            let mut scratch: Vec<i64> = []
            for i in 0..1000 { scratch.push(i * round) }

            let mut sum = 0
            for x in scratch { sum += x }
            grand_total += sum      // a scalar survives; the Vec does not
        }
    }
    println!("grand total = {grand_total}")
}
`,
  },
  {
    slug: "defer",
    title: "defer for cleanup",
    prose: `
      <p><code>defer expr</code> schedules work to run when control
      leaves the enclosing block by <strong>any</strong> path -
      fall-through, <code>return</code>, <code>break</code>, or
      <code>continue</code> - in <strong>LIFO</strong> order. It keeps a
      resource's release next to its acquisition instead of scattered
      across every exit.</p>
      <p>In a loop body it runs once per iteration. It is the same
      pattern Go uses: open, <code>defer</code> the close, then use - and
      the close is guaranteed to happen.</p>`,
    code: `fn use_resource(name: &String) {
    println!("  open {name}")
    defer println!("  close {name}")     // runs on every exit path
    println!("  use {name}")
}

fn main() {
    // \`defer\` runs when control leaves the block, in LIFO order.
    {
        defer println!("third")
        defer println!("second")
        defer println!("first")
        println!("body runs")
    }
    println!("---")
    use_resource(&"file")
}
`,
  },
  {
    slug: "together",
    title: "A small program",
    prose: `
      <p>One small program, every idea at once: immutable bindings, a
      <code>HashMap</code> counter, iteration with tuple destructuring, a
      <code>#[derive]</code>d struct, a descending <code>sort_by_key</code>
      with <code>Reverse</code>, and an aligned format spec.</p>
      <p>It counts word frequencies, moves the entries into <code>Tally</code>
      structs, sorts by count, and prints a tidy table. Edit the input text
      and run it again - that is the whole language in twenty lines. Go build
      something.</p>`,
    code: `#[derive(Debug)]
struct Tally { word: String, count: i64 }

fn main() {
    let text = "go rust go gossamer rust go"

    // Count each word; \`inc\` does get-or-zero then add.
    let mut counts = {}
    for word in text.split(" ") {
        counts.inc(word)
    }

    // Move the entries into structs and sort by count, descending.
    let mut rows = []
    for (word, count) in counts.iter() {
        rows.push(Tally { word: word, count: count })
    }
    rows.sort_by_key(|r| Reverse(r.count))

    for r in rows {
        println!("{:>9} x {}", r.word, r.count)
    }
}
`,
  },
];

// ---- lazy playground loader ------------------------------------------
let playgroundPromise = null;

/// Import the playground module once and resolve its mount function.
function loadMountPlayground() {
  if (!playgroundPromise) {
    playgroundPromise = import(PLAYGROUND_URL).then((m) => m.mountPlayground);
  }
  return playgroundPromise;
}

// ---- small DOM helper ------------------------------------------------
/// Create an element with an optional class and appended children.
function el(tag, className, ...children) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  for (const child of children) {
    if (child != null) node.append(child);
  }
  return node;
}

// ---- controller state ------------------------------------------------
let activeIndex = -1;
let playground = null;
let firstRender = true;
let refs = {};

/// Index of the lesson whose slug matches the current hash, or 0.
function indexFromHash() {
  const slug = location.hash.replace(/^#/, "");
  const i = LESSONS.findIndex((l) => l.slug === slug);
  return i >= 0 ? i : 0;
}

/// Move to a lesson by index; the hash is the single source of truth.
function navigateTo(index) {
  const clamped = Math.max(0, Math.min(LESSONS.length - 1, index));
  const slug = LESSONS[clamped].slug;
  if (location.hash === "#" + slug) {
    syncFromHash();
  } else {
    location.hash = slug;
  }
}

/// Render whatever lesson the hash names, unless it is already shown.
function syncFromHash() {
  const i = indexFromHash();
  if (i === activeIndex) return;
  showLesson(i);
}

/// A spinner shown in the playground host while the module loads.
function loadingNode() {
  return el(
    "div",
    "tour-play-loading",
    el("span", "spin"),
    document.createTextNode("Loading the playground..."),
  );
}

/// A static code listing shown when the playground cannot mount.
function fallbackNode(code) {
  const pre = el("pre");
  pre.textContent = code;
  const note = el("p", "note");
  note.innerHTML =
    "The interactive editor could not load. Copy this program and run it with <code>gos</code>.";
  return el("div", "tour-play-fallback", pre, note);
}

/// Tear down the previous lesson, render the new one, and (re)mount the
/// playground. Guards against a stale async mount when the user advances
/// before the module resolves.
async function showLesson(index) {
  if (playground) {
    playground.destroy();
    playground = null;
  }
  activeIndex = index;
  const lesson = LESSONS[index];

  refs.stepButtons.forEach((btn, i) => {
    if (i === index) btn.setAttribute("aria-current", "step");
    else btn.removeAttribute("aria-current");
  });

  refs.prose.innerHTML = `<h1>${lesson.title}</h1>${lesson.prose}`;

  const human = index + 1;
  const total = LESSONS.length;
  refs.progressText.textContent = `${human} / ${total}`;
  refs.progressFill.style.width = `${(human / total) * 100}%`;
  refs.centerText.textContent = `${human} / ${total}`;
  refs.prevBtn.disabled = index === 0;
  refs.nextBtn.disabled = index === total - 1;
  document.title = `${lesson.title} - A Tour of Gossamer`;

  // Move focus (and the viewport) to the new heading on user navigation,
  // but never steal focus on the initial page load.
  if (!firstRender) {
    const heading = refs.prose.querySelector("h1");
    if (heading) {
      heading.tabIndex = -1;
      heading.focus();
    }
  }
  firstRender = false;

  const host = refs.play;
  host.replaceChildren(loadingNode());
  try {
    const mount = await loadMountPlayground();
    if (activeIndex !== index) return;
    playground = mount(host, { source: lesson.code, height: PLAY_HEIGHT });
  } catch (err) {
    if (activeIndex !== index) return;
    host.replaceChildren(fallbackNode(lesson.code));
  }
}

/// Roving arrow-key navigation within the step list.
function onStepsKeydown(e) {
  const btns = refs.stepButtons;
  const idx = btns.indexOf(document.activeElement);
  if (idx < 0) return;
  let next = -1;
  switch (e.key) {
    case "ArrowDown":
    case "ArrowRight":
      next = Math.min(btns.length - 1, idx + 1);
      break;
    case "ArrowUp":
    case "ArrowLeft":
      next = Math.max(0, idx - 1);
      break;
    case "Home":
      next = 0;
      break;
    case "End":
      next = btns.length - 1;
      break;
    default:
      return;
  }
  e.preventDefault();
  btns[next].focus();
}

/// Build the sidebar, content shell, and controls once, caching refs.
function buildShell(root) {
  root.replaceChildren();

  const sidebar = el("aside", "tour-sidebar");
  const heading = el("h2");
  heading.id = "tour-steps-label";
  heading.textContent = "Lessons";
  const steps = el("ul", "tour-steps");
  steps.setAttribute("aria-labelledby", "tour-steps-label");
  const stepButtons = [];
  LESSONS.forEach((lesson, i) => {
    const btn = el("button", "step-btn");
    btn.type = "button";
    btn.textContent = lesson.title;
    btn.addEventListener("click", () => navigateTo(i));
    steps.append(el("li", null, btn));
    stepButtons.push(btn);
  });
  steps.addEventListener("keydown", onStepsKeydown);
  sidebar.append(heading, steps);

  const content = el("section", "tour-content");

  const progressText = el("span");
  const progressFill = el("i");
  const bar = el("div", "bar", progressFill);
  const progress = el("div", "tour-progress", progressText, bar);

  const prose = el("article", "tour-prose");
  const play = el("div", "tour-play");
  const split = el("div", "tour-split", prose, play);

  const prevBtn = el("button", "nav-btn");
  prevBtn.type = "button";
  prevBtn.innerHTML = '<span aria-hidden="true">&larr;</span> Prev';
  prevBtn.addEventListener("click", () => navigateTo(activeIndex - 1));

  const centerText = el("span", "center");

  const nextBtn = el("button", "nav-btn primary");
  nextBtn.type = "button";
  nextBtn.innerHTML = 'Next <span aria-hidden="true">&rarr;</span>';
  nextBtn.addEventListener("click", () => navigateTo(activeIndex + 1));

  const controls = el("div", "tour-controls", prevBtn, centerText, nextBtn);

  content.append(progress, split, controls);
  root.append(sidebar, content);

  refs = {
    stepButtons,
    prose,
    play,
    progressText,
    progressFill,
    centerText,
    prevBtn,
    nextBtn,
  };
}

/// Last-resort message if the page cannot be assembled at all.
function renderFatal(root, err) {
  const p = el("p", "tour-fatal");
  p.textContent =
    "The tour failed to load: " +
    (err && err.message ? err.message : String(err));
  root.replaceChildren(p);
}

function init() {
  const root = document.getElementById("tour-root");
  if (!root) return;
  try {
    buildShell(root);
    window.addEventListener("hashchange", syncFromHash);
    syncFromHash();
  } catch (err) {
    renderFatal(root, err);
  }
}

init();
