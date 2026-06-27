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
      straight from scope by name - <code>{name}</code> - and the six
      format macros are the only macros in the language.</p>
      <p>Press <strong>Run</strong> (or Ctrl / Cmd + Enter) to execute the
      program on the right. Edit it freely and run it again.</p>`,
    code: `// Bindings are immutable by default; reach for \`let mut\` only when
// a value really changes. String literals are already \`String\`.
let name = "Gossamer"
let version = 0.18

let greeting = "hello, " + &name
println!("{greeting}!")

// Named interpolation reads bindings straight from scope.
println!("{name} is {} bytes long", name.len())
println!("you are running version {version}")
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
      receiver: <code>x |> _.trim</code> is <code>x.trim()</code>. The
      data-last std helpers such as <code>iter::filter</code> and
      <code>iter::sum_by</code> were shaped to chain through <code>|></code>
      with no placeholder at all.</p>`,
    code: `use std::iter

fn double(x: i64) -> i64 { x * 2 }
fn add(a: i64, b: i64) -> i64 { a + b }

// \`x |> f\` is \`f(x)\`; \`x |> f(a)\` lands x in the last slot: \`f(a, x)\`.
let n = 3 |> double |> add(10)
println!("3 |> double |> add(10) = {n}")

// \`_.method\` pipes a value through its own methods - \`_\` is the receiver.
let shout = "  hi there  " |> _.trim |> _.to_upper
println!("shout = {shout}")

// Data-last helpers read top-down, with no \`let mut\` accumulator.
let total = iter::range_inclusive(1, 5)
    |> iter::filter(|n: i64| n % 2 == 1)
    |> iter::sum_by(|n: i64| n)
println!("sum of odds in 1..=5 = {total}")
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
        Shape::Circle(r) => 3.14159 * r * r,
        Shape::Rect { w, h } => w * h,
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
    slug: "errors",
    title: "Error handling",
    prose: `
      <p>Gossamer has no exceptions. Fallible functions return
      <code>Result&lt;T, E&gt;</code> and <code>?</code> propagates the
      <code>Err</code> branch upward. Build and chain typed errors with
      <code>std::errors</code>: <code>errors::new</code>,
      <code>errors::wrap</code> for higher-level context, and printing a
      wrapped error shows the colon-joined cause chain.</p>
      <p>On the ok-path, <code>result::map</code> transforms <code>Ok</code>
      and leaves <code>Err</code> untouched, while
      <code>result::default_with</code> handles the error in-line. Both are
      data-last, so the whole flow threads through <code>|></code>.</p>`,
    code: `use std::errors
use std::result

// Fallible work returns \`Result<T, E>\`; \`?\` propagates the \`Err\`.
fn parse_port(text: &String) -> Result<i64, errors::Error> {
    let n: i64 = text.parse().map_err(|_| errors::new(format!("not a number: {text}")))?
    if n <= 0 { return Err(errors::new(format!("must be positive: {n}"))) }
    Ok(n)
}

fn main() {
    // Map the Ok value; handle the Err in-line - both thread through |>.
    parse_port(&"8080")
        |> result::map(|n| println!("port = {n}"))
        |> result::default_with(|e| eprintln!("error: {}", e.message()))

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
      Iterator pipelines compose with <code>|></code> instead of dropping to
      a manual loop.</p>`,
    code: `use std::collections::HashMap
use std::iter

fn main() {
    // A growable Vec; iterate the values directly.
    let mut nums: [i64] = [4, 8, 15, 16, 23]
    nums.push(42)
    println!("count = {}, last = {}", nums.len(), nums[nums.len() - 1])

    // Sum the even values with a data-last pipeline.
    let even_sum = nums
        |> iter::filter(|n: i64| n % 2 == 0)
        |> iter::sum_by(|n: i64| n)
    println!("sum of evens = {even_sum}")

    // HashMap counters: \`inc\` does the get-or-zero-then-add for you.
    let mut tally: HashMap<String, i64> = HashMap::new()
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
      and hand off mid-run - runs natively with <code>gos run</code>.</p>`,
    code: `use std::sync::channel

// The producer sends every value, then closes - it runs to
// completion, so no mid-run hand-off is needed.
fn produce(tx: Sender<i64>) {
    for n in 1..=5 { tx.send(n * n) }
    tx.close()
}

fn main() {
    let (tx, rx) = channel()
    go produce(tx)

    // \`recv\` yields \`Some\` until the channel is closed and drained.
    let mut total = 0
    while let Some(v) = rx.recv() { total += v }
    println!("sum of squares 1..=5 = {total}")
}
`,
  },
  {
    slug: "types",
    title: "Structs / traits / generics / derive",
    prose: `
      <p>Structs are runtime-managed value types; traits define a shared
      interface that each type implements. <code>#[derive(...)]</code>
      synthesizes <code>Debug</code>, equality, <code>clone</code>, and
      <code>Default</code> as real code, so <code>==</code>,
      <code>{:?}</code>, and <code>.clone()</code> just work on every tier.</p>
      <p>Generic functions take trait bounds -
      <code>fn farther&lt;T: Distance&gt;(...)</code> - and each call site
      monomorphises to a direct call, with no dynamic dispatch.</p>`,
    code: `// \`#[derive(...)]\` synthesizes Debug / equality / clone as real code.
#[derive(Clone, PartialEq, Debug)]
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
    slug: "together",
    title: "A small program",
    prose: `
      <p>One small program, every idea at once: immutable bindings, a
      <code>HashMap</code> counter, iteration with tuple destructuring, a
      <code>#[derive]</code>d struct, a descending <code>sort_by</code>, and
      an aligned format spec.</p>
      <p>It counts word frequencies, moves the entries into <code>Tally</code>
      structs, sorts by count, and prints a tidy table. Edit the input text
      and run it again - that is the whole language in twenty lines. Go build
      something.</p>`,
    code: `use std::collections::HashMap

#[derive(Debug)]
struct Tally { word: String, count: i64 }

fn main() {
    let text = "go rust go gossamer rust go"

    // Count each word; \`inc\` does get-or-zero then add.
    let mut counts: HashMap<String, i64> = HashMap::new()
    for word in text.split(" ") {
        counts.inc(word)
    }

    // Move the entries into structs and sort by count, descending.
    let mut rows: [Tally] = []
    for (word, count) in counts.iter() {
        rows.push(Tally { word: word, count: count })
    }
    rows.sort_by(|a, b| b.count - a.count)

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
    "The interactive editor could not load. Copy this program and run it with <code>gos run</code>.";
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
