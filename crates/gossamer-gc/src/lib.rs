//! Garbage collector for the Gossamer runtime.
//! Provides a stop-the-world mark-sweep heap keyed by safe [`GcRef`]
//! handles. Later phases will layer a concurrent mark phase on top
//!; this initial version establishes the alloc/trace/sweep
//! contract and the statistics that downstream components need.

#![forbid(unsafe_code)]

mod heap;
mod weak;

pub use heap::{ConcurrentPhase, GcConfig, GcRef, GcStats, Heap, Obj, ObjKind};
pub use weak::{FinaliserFn, FinalizerSet, InternTable, WeakRef, WeakTable};
