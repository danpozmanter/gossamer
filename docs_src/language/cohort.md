# `lang::cohort`

Structured concurrency: `cohort { }` owns the goroutines `spawn`ed inside it, joins them on every exit path, and reports the first failure as its `Result`.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

A `cohort` block owns the goroutines started inside it. The block cannot
be left until every one of them has finished, so a background task cannot
outlive the code that started it, and a failure in one of them cannot
vanish unread.

```gossamer
use std::errors

fn fetch(url: String) -> Result<String, errors::Error> {
    // ... network work ...
    Ok(url)
}

fn gather() -> Result<(), errors::Error> {
    cohort {
        let a = spawn(|| fetch("one"))
        let b = spawn(|| fetch("two"))
        println!("{} {}", a.join()??, b.join()??)
    }
}

fn main() {
    println!("{:?}", gather())
}
```

The block is an expression. Its value is `Result<(), errors::Error>`, so
it binds like any other fallible call and composes with `?`:

```gossamer
fn stage() -> Result<(), errors::Error> {
    cohort {
        let _a = spawn(|| fetch("one"))
        let _b = spawn(|| fetch("two"))
    }?
    Ok(())
}
```

## Two ways to start a goroutine

Gossamer has both, and they mean different things.

| | `spawn(f)` inside a `cohort` | `go f()` |
|---|---|---|
| Lifetime | bounded by the block | unbounded |
| On failure | cancels siblings, becomes the block's `Err` | lost |
| Joined | by the block, always | never |
| Handle | `JoinHandle<T>` for the value | none |

```gossamer
// Structured: the block waits, and reports what went wrong.
let outcome = cohort {
    let _a = spawn(|| index_shard(0))
    let _b = spawn(|| index_shard(1))
}

// Detached: fire-and-forget, for work with no relationship to the
// code that started it.
go metrics_reporter()
```

Reach for `cohort` by default. `go` is right when a goroutine genuinely
should outlive the block that started it - a background reporter, a
supervisor loop - and nothing is waiting on its result. A `go` written
*inside* a cohort is almost always a mistake, and `gos lint` reports it
as `GL0053`.

## `main` is already a cohort

Every program's `main` runs inside an implicit root cohort, so every
`spawn` belongs to one even without a block written around it. Two
things follow:

- No goroutine outlives the program.
- A spawned goroutine that fails, and whose handle nobody joins, is
  reported on stderr at exit instead of disappearing:

```
gossamer: spawned goroutine failed with nobody to observe it: connection refused
```

The root cohort's policy is collect-all, so one failing goroutine never
cancels another's work. Writing `cohort { }` explicitly is how you ask
for a tighter boundary than "the whole program".

## Failure and cancellation

A child fails by panicking or by answering `Err`. By default the first
failure cancels its siblings and becomes the block's error:

```gossamer
let outcome = cohort {
    let _a = spawn(|| work(1))
    let _b = spawn(|| work(2))   // returns Err
    let _c = spawn(|| work(3))   // cancelled
}
// outcome is Err(..) once every child has stopped
```

The reported failure is the one with the lowest spawn index, never
whichever child happened to finish first, so the answer is the same on
every run and on every execution tier.

Cancellation is cooperative, and nothing is killed. A cancelled child
sees it as an operation's ordinary "nothing more is coming" answer: a
`recv` reports `None`, exactly as a closed channel does, and a `sleep`
returns early. The child then leaves through its own normal exit path,
so its `defer` frames and destructors run in order:

```gossamer
fn worker(rx: Receiver<Job>) -> Result<(), errors::Error> {
    defer println!("worker stopped")
    // Ends on cancellation the same way it ends on a closed channel.
    while let Some(job) = rx.recv() {
        handle(job)?
    }
    Ok(())
}
```

Pure computation is not a cancellation point. A CPU-bound child decides
where it is willing to stop:

```gossamer
use std::runtime

fn search(space: Vec<Board>) -> Result<i64, errors::Error> {
    let mut best = 0
    for board in space {
        if runtime::cohort_cancelled() {
            break
        }
        best = max(best, evaluate(board))
    }
    Ok(best)
}
```

## Settings

```gossamer
cohort(policy: Policy::CollectAll) { ... }
cohort(timeout: 500) { ... }
cohort(context: Context::Isolated) { ... }
```

- **`policy:`** - `Policy::FailFast` (the default) stops at the first
  failure. `Policy::CollectAll` runs every child and reports all their
  failures. `Policy::Race` stops at the first success; the losers are
  cancelled, and work they already committed is not undone, so `race` is
  not a transaction.
- **`timeout:`** - milliseconds. The cohort is cancelled when the
  deadline passes, and the block reports the timeout as its error.
- **`context:`** - `Context::Isolated` gives each child a dedicated OS
  thread for its whole life. That is what a synchronous Rust FFI call or
  never-yielding CPU-bound work needs: neither can be interrupted, and on
  a shared carrier they would stall every goroutine that carrier is
  running. Stdlib blocking calls need no such treatment - the runtime
  already moves them off the carrier. Channels work across contexts
  unchanged.

## Nesting

Cohorts nest, and a nested one is joined before the block containing it
continues. Cancelling the outer cohort cancels the inner one through the
same chain a child's own check walks.

```gossamer
let outcome = cohort {
    let _top = spawn(|| stage_one())
    cohort {
        let _a = spawn(|| stage_two(0))
        let _b = spawn(|| stage_two(1))
    }?
}
```

## What a cohort does not do

- It does not kill anything. A child that performs no cancellation point
  and never returns will keep the block waiting - the same property Go,
  Kotlin, and Java's virtual threads have.
- It does not roll anything back. `Policy::Race` cancels the losers; it
  does not undo the writes they already made.
- It does not make a child's value appear in the block's result. Values
  come back through `JoinHandle::join` or a channel; the cohort's own
  value reports success or the failure that stopped it.
