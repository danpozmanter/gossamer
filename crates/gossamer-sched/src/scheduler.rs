//! Cooperative scheduler driving [`crate::Task`] state machines.

#![forbid(unsafe_code)]

use crate::queue::RunQueue;
use crate::task::{Gid, Step, Task};

/// Statistics surfaced after the scheduler has drained its run queue.
#[derive(Debug, Clone, Copy, Default)]
pub struct SchedStats {
    /// Total goroutines spawned since construction.
    pub spawned: u64,
    /// Total goroutines that ran to completion.
    pub finished: u64,
    /// Total `Task::step` invocations across every goroutine.
    pub steps: u64,
    /// Total [`Step::Yield`] returns observed.
    pub yields: u64,
}

/// Cooperative M:N scheduler. A single scheduler maps many goroutines
/// onto the one thread that drives [`Self::run`]; will
/// broaden the model to multiple machines with work stealing.
#[derive(Default)]
pub struct Scheduler {
    tasks: Vec<Slot>,
    queue: RunQueue,
    next_id: u32,
    stats: SchedStats,
}

enum Slot {
    Active(Box<dyn Task>),
    Finished,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("Scheduler")
            .field("active", &self.active_count())
            .field("queued", &self.queue.len())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Scheduler {
    /// Constructs an empty scheduler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawns a new goroutine driven by `task` and enqueues it for
    /// execution. Returns the [`Gid`] identifying the new goroutine.
    pub fn spawn(&mut self, task: impl Task + 'static) -> Gid {
        let gid = Gid(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("too many goroutines spawned");
        self.tasks.push(Slot::Active(Box::new(task)));
        self.queue.push(gid);
        self.stats.spawned = self.stats.spawned.saturating_add(1);
        gid
    }

    /// Returns `true` when `gid` is still scheduled (either queued or
    /// currently parked).
    #[must_use]
    pub fn is_active(&self, gid: Gid) -> bool {
        matches!(self.slot(gid), Some(Slot::Active(_)))
    }

    /// Advances every active goroutine FIFO until every one has
    /// finished. Returns the number of step invocations performed.
    pub fn run(&mut self) -> u64 {
        let mut steps: u64 = 0;
        while let Some(gid) = self.queue.pop() {
            if !matches!(self.slot(gid), Some(Slot::Active(_))) {
                continue;
            }
            let step = self.step_task(gid);
            steps = steps.saturating_add(1);
            self.stats.steps = self.stats.steps.saturating_add(1);
            match step {
                Step::Yield => {
                    self.stats.yields = self.stats.yields.saturating_add(1);
                    self.queue.push(gid);
                }
                Step::Done => {
                    self.finish(gid);
                }
            }
        }
        steps
    }

    /// Advances by at most `budget` step invocations. Returns the
    /// number of steps actually performed; the caller can poll this to
    /// run the scheduler in bounded quanta.
    pub fn run_bounded(&mut self, budget: u64) -> u64 {
        let mut performed: u64 = 0;
        while performed < budget {
            let Some(gid) = self.queue.pop() else {
                break;
            };
            if !matches!(self.slot(gid), Some(Slot::Active(_))) {
                continue;
            }
            let step = self.step_task(gid);
            performed += 1;
            self.stats.steps = self.stats.steps.saturating_add(1);
            match step {
                Step::Yield => {
                    self.stats.yields = self.stats.yields.saturating_add(1);
                    self.queue.push(gid);
                }
                Step::Done => self.finish(gid),
            }
        }
        performed
    }

    /// Returns the number of goroutines currently known to the
    /// scheduler (active or finished-but-not-collected).
    #[must_use]
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Returns `true` when there are no known goroutines.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.active_count() == 0
    }

    /// Current snapshot of scheduler statistics.
    #[must_use]
    pub fn stats(&self) -> SchedStats {
        self.stats
    }

    /// Returns the number of goroutines still in the active state.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|slot| matches!(slot, Slot::Active(_)))
            .count()
    }

    fn slot(&self, gid: Gid) -> Option<&Slot> {
        self.tasks.get(gid.0 as usize)
    }

    fn step_task(&mut self, gid: Gid) -> Step {
        let slot = self
            .tasks
            .get_mut(gid.0 as usize)
            .expect("unknown goroutine in run queue");
        match slot {
            Slot::Active(task) => task.step(),
            Slot::Finished => Step::Done,
        }
    }

    fn finish(&mut self, gid: Gid) {
        if let Some(slot) = self.tasks.get_mut(gid.0 as usize) {
            *slot = Slot::Finished;
            self.stats.finished = self.stats.finished.saturating_add(1);
        }
    }
}
