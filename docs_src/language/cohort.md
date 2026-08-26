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
        println("{} {}", a.join()??, b.join()??)
    }
}

fn main() {
    println("{:?}", gather())
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

## One way to start a goroutine

`spawn(f)` is it. There is no detached form: every goroutine attaches to
the enclosing cohort, so the block waits for it, reports what went wrong,
and hands back a `JoinHandle<T>` for the value.

```gossamer
let outcome = cohort {
    let _a = spawn(|| index_shard(0))
    let _b = spawn(|| index_shard(1))
}
```

A closure body runs on the child, so an operand that has to be read where
the spawn is written is bound first:

```gossamer
let shard = shards[i]
spawn(|| index_shard(shard))
```

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
    defer println("worker stopped")
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

## Scheduling and preemption

Scheduling is cooperative. A goroutine hands its worker back at a
*safepoint* - a channel or `select` operation, a mutex, `time::sleep`, a
scheduler-aware read, and every function call and scheduler step - and a
watchdog asks a long-running worker to yield at the next one. Nothing
relocates a running goroutine off its worker asynchronously, as Go's signal
handler does.

That distinction shows up in exactly one shape: a CPU-bound loop that calls
nothing at all. The bytecode VM still yields on such a loop's back-edges, but a
`gos build` binary does not - the compiled backends leave loop back-edges
un-polled so the optimizers can keep numeric loops tight. Such a loop holds one
worker until it finishes; its peers keep running on the others.

The fix is the same one cancellation already asks for: give a long computation
a point where it is willing to stop. The `runtime::cohort_cancelled()` check
above is one, and so is any ordinary call on an outer iteration.

## Settings

```gossamer
cohort(policy: Policy::CollectAll) { ... }
cohort(timeout: 500) { ... }
cohort(isolation: Isolation::Thread) { ... }
cohort(on_error: OnError::Log) { ... }
cohort(cancellable: false) { ... }
cohort(drain: 2000) { ... }
```

- **`policy:`** - `Policy::FailFast` (the default) stops at the first
  failure. `Policy::CollectAll` runs every child and reports all their
  failures. `Policy::Race` stops at the first success; the losers are
  cancelled, and work they already committed is not undone, so `race` is
  not a transaction.
- **`timeout:`** - milliseconds. The cohort is cancelled when the
  deadline passes, and the block reports the timeout as its error.
- **`on_error:`** - what the cohort does with a child's failure, where
  `policy:` decides when it stops waiting. `OnError::Propagate` (the
  default) makes the first failure the block's `Err`. `OnError::Log` names
  every failure on stderr as it happens and the block answers `Ok`.
  `OnError::Ignore` answers `Ok` and says nothing. None of them makes a
  child unaccountable: it is still counted, still drained, and a child that
  never finishes is still named by the drain report.
- **`cancellable:`** - `false` exempts the cohort and everything under it
  from cancellation, which is what a shielded region asks for. The
  exemption covers cancellation only; the block still drains and reports.
- **`drain:`** - milliseconds to wait for the children once the body is
  done. Distinct from `timeout:`, which bounds the body's own work. Without
  it the block waits as long as its children take, because leaving the
  block is the program's statement that they are finished; a drain that
  gives up names what it left running.
- **`isolation:`** - `Isolation::Thread` gives each child a dedicated OS
  thread for its whole life; `Isolation::Shared` is the default. That is what a synchronous Rust FFI call or
  never-yielding CPU-bound work needs: neither can be interrupted, and on
  a shared carrier they would stall every goroutine that carrier is
  running. Stdlib blocking calls need no such treatment - the runtime
  already moves them off the carrier. Channels work across both
  unchanged. The retired `context:` spelling reports `GP0056` with the
  rewrite, and `--fix` applies it.

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
