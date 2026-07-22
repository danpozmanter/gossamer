//! Runtime ownership for finalized in-process JIT allocations.
//!
//! Cranelift owns the provider adapter only while a module is being built.
//! The adapter and the runtime artifact share this heap, so dropping the
//! module releases compiler metadata without invalidating finalized entries.

#![allow(unsafe_code)]

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use cranelift_jit::{BranchProtection, JITMemoryKind, JITMemoryProvider, SystemMemoryProvider};
use cranelift_module::ModuleResult;
use parking_lot::Mutex;

const BUILDING: u8 = 0;
const FINALIZED: u8 = 1;
const DETACHED: u8 = 2;
const RETIRED: u8 = 3;

/// Shared owner of every mapping referenced by finalized native code.
///
/// The mutex is used only for allocation, finalization, and destruction.
/// Calling a finalized entry does not touch this object or acquire a lock.
pub(crate) struct NativeCodeHeap {
    memory: Mutex<SystemMemoryProvider>,
    state: AtomicU8,
}

impl NativeCodeHeap {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            memory: Mutex::new(SystemMemoryProvider::new()),
            state: AtomicU8::new(BUILDING),
        })
    }

    pub(crate) fn provider(heap: &Arc<Self>) -> GossamerJitMemoryProvider {
        GossamerJitMemoryProvider {
            heap: Arc::clone(heap),
        }
    }

    /// Marks the point after the module and all module-owned metadata have
    /// been destroyed. Entry pointers may be published only after this call.
    pub(crate) fn mark_detached(&self) {
        let previous =
            self.state
                .compare_exchange(FINALIZED, DETACHED, Ordering::AcqRel, Ordering::Acquire);
        debug_assert_eq!(previous, Ok(FINALIZED));
    }

    pub(crate) fn is_detached(&self) -> bool {
        self.state.load(Ordering::Acquire) == DETACHED
    }
}

impl Drop for NativeCodeHeap {
    fn drop(&mut self) {
        let previous = self.state.swap(RETIRED, Ordering::AcqRel);
        debug_assert!(matches!(previous, BUILDING | FINALIZED | DETACHED));
        // SAFETY: the heap can be dropped only after the provider adapter and
        // every artifact owner are gone. Gossamer keeps the artifact alive
        // while an entry is reachable and tears worker VMs down only after
        // their native calls complete.
        unsafe { self.memory.get_mut().free_memory() };
    }
}

/// Temporary adapter owned by `JITModule` during compilation.
pub(crate) struct GossamerJitMemoryProvider {
    heap: Arc<NativeCodeHeap>,
}

impl JITMemoryProvider for GossamerJitMemoryProvider {
    fn allocate(&mut self, size: usize, align: u64, kind: JITMemoryKind) -> io::Result<*mut u8> {
        if self.heap.state.load(Ordering::Acquire) != BUILDING {
            return Err(io::Error::other(
                "cannot allocate into a finalized Gossamer JIT artifact",
            ));
        }
        self.heap.memory.lock().allocate(size, align, kind)
    }

    unsafe fn free_memory(&mut self) {
        self.heap.state.store(RETIRED, Ordering::Release);
        // SAFETY: this method carries the provider trait's caller contract.
        unsafe { self.heap.memory.lock().free_memory() };
    }

    fn finalize(&mut self, branch_protection: BranchProtection) -> ModuleResult<()> {
        if self.heap.state.load(Ordering::Acquire) != BUILDING {
            return Err(cranelift_module::ModuleError::Backend(anyhow::anyhow!(
                "Gossamer JIT artifact was already finalized"
            )));
        }
        self.heap.memory.lock().finalize(branch_protection)?;
        self.heap.state.store(FINALIZED, Ordering::Release);
        Ok(())
    }
}
