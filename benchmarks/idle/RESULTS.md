# Idle CPU baseline

`benchmarks/idle/check.sh` runs `gos run empty.gos`, which calls
`time::sleep(2000)` and exits. The total user+system CPU consumed
during those two wall-clock seconds is the regression signal.

## Latest run (2026-04-28, post-Track-A)

| metric            | value     |
| ----------------- | --------- |
| wall clock        | 2.000 s   |
| user CPU          | 0.001 s   |
| system CPU        | 0.001 s   |
| **total CPU**     | **2 ms**  |
| CI gate threshold | 400 ms    |

A pre-Track-A baseline (no LH5/LH8/D2/D3) measured ~1500 ms total
CPU on the same machine, almost entirely from the tree-walker
scheduler busy-polling and the seven 1 kHz signal-relay threads.
The 750x reduction is the headline result of Track A.

If this number creeps back up, the suspects are roughly in order:

1. A new busy-poll loop in the scheduler (check
   `MultiScheduler::wait_until_idle` and `eval_select`).
2. A signal handler being polled instead of blocked on
   `Signals::forever()` (check `gossamer_std::signal` and
   `gossamer_runtime::preempt`).
3. A new netpoller cycle below ~50 ms (check
   `gossamer_std::sched_global::poller_loop`).
