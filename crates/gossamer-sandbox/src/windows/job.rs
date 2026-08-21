//! Job objects: process-tree lifetime and resource limits.
//!
//! A job object is tree and resource control, never a security
//! boundary on its own, so it never counts toward a level by itself.
//! What it buys is the guarantee that closing the job handle ends every
//! process in it, which is how `kill_tree_on_exit` is honored on
//! Windows.

#![allow(
    unsafe_code,
    reason = "job objects are a raw Win32 API; every call below passes a \
              stack-owned struct with its size, which is the documented \
              contract"
)]

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_LIMIT_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};

use crate::policy::Resources;

/// A job object that kills everything in it when dropped.
pub(crate) struct Job(HANDLE);

// The handle is owned by this value and is only used from the thread
// that holds it; Win32 handles are process-wide and safe to move.
#[allow(
    unsafe_code,
    reason = "a Win32 HANDLE is a process-wide token with no thread affinity"
)]
unsafe impl Send for Job {}

impl Job {
    /// Creates an anonymous job with the policy's limits applied.
    ///
    /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` is what makes the tree
    /// teardown reliable: a descendant that outlives its parent is
    /// still in the job, so closing the handle ends it.
    pub(crate) fn create(resources: &Resources) -> Result<Self, String> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(format!(
                "CreateJobObject failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let job = Self(handle);
        job.apply_limits(resources)?;
        Ok(job)
    }

    fn apply_limits(&self, resources: &Resources) -> Result<(), String> {
        let mut flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let mut basic = JOBOBJECT_BASIC_LIMIT_INFORMATION {
            LimitFlags: 0,
            ..unsafe { std::mem::zeroed() }
        };
        if let Some(count) = resources.max_processes {
            flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            basic.ActiveProcessLimit = count;
        }
        let mut extended = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            BasicLimitInformation: basic,
            ..unsafe { std::mem::zeroed() }
        };
        if let Some(bytes) = resources.max_memory {
            flags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
            extended.JobMemoryLimit = usize::try_from(bytes).unwrap_or(usize::MAX);
        }
        extended.BasicLimitInformation.LimitFlags = flags;
        let applied = unsafe {
            SetInformationJobObject(
                self.0,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&extended).cast(),
                u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                    .unwrap_or(0),
            )
        };
        if applied == 0 {
            return Err(format!(
                "SetInformationJobObject failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    /// Puts `process` and every descendant it starts into the job.
    pub(crate) fn assign(&self, process: HANDLE) -> Result<(), String> {
        if unsafe { AssignProcessToJobObject(self.0, process) } == 0 {
            return Err(format!(
                "AssignProcessToJobObject failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // Closing the last handle to a kill-on-close job terminates
        // every process still in it, which is the tree teardown.
        unsafe { CloseHandle(self.0) };
    }
}
