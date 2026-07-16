# Memory model

Status: shipped

Gossamer programs are expected to be data-race free. When a program
shares mutable heap state across goroutines, it must order access with
channels, `std::sync` primitives, or atomics.

For race-free programs, each goroutine observes memory as if goroutines
were interleaved at synchronization points. A data race is a pair of
conflicting accesses to the same memory location from different
goroutines, at least one of them a write, with no happens-before edge
between them.

## Happens-before edges

The runtime establishes synchronization for these operations:

- Starting a goroutine happens before that goroutine begins executing.
- A channel send synchronizes with the receive that obtains that value.
- Closing a channel synchronizes with receives that observe the closed
  and drained state.
- `select` uses the same send, receive, and close edges as the selected
  operation.
- `Mutex` unlock synchronizes with a later lock that observes the
  release.
- `RwLock` write unlock synchronizes with later readers or writers that
  acquire the lock.
- `WaitGroup::done` synchronizes with a `wait` that observes the count
  reach zero.
- `Once::call_once` publishes the completed initialization body to every
  caller that returns from the same `Once`.
- Atomic operations synchronize according to the operation's documented
  ordering. Sequentially consistent operations and release-store/acquire-load
  pairs establish detector-visible edges. Relaxed operations are atomic but do
  not establish an ordering edge.

## Channels

`channel()` and `channel(0)` create an unbuffered rendezvous channel.
A send on an unbuffered channel does not complete until a receiver
accepts that exact value.

`channel(n)` for `n > 0` creates a bounded buffered channel. Sends block
when the buffer is full. Receives block when the buffer is empty.

`channel::unbounded()` creates an explicit unbounded queue channel for
code that intentionally wants producer sends to complete without a
receiver or capacity limit.

Receiving from a closed and drained channel returns `None`. Closing an
already-closed channel is an error.

## Select

When multiple non-default `select` arms are ready, Gossamer polls arms in
pseudo-random order so source order does not deterministically win.
`default` is selected only when no send or receive arm is ready.

Blocking `select` registers interest in all channel arms and wakes when
any of those channels may have become ready.

## Race detector

`gos test --race` instruments compiled test code and reports
unsynchronized heap access pairs. The detector tracks vector clocks and
the synchronization edges above. It is intended to catch real program
races, but it is not a substitute for the memory model: code that shares
mutable state should still use channels, locks, wait groups, or atomics.
