//! `AppContainer`: the `strict` level on Windows.
//!
//! The container's package SID is derived from a stable name rather
//! than generated, so a run that crashes leaves grants that can be
//! found and removed. Every granted path gets an ACE for that SID
//! before the run and loses it after; the record in [`super::acl`] is
//! what makes the "after" survive a crash.

#![allow(
    unsafe_code,
    reason = "AppContainer profiles and ACL entries are a raw Win32 API"
)]

use std::process::Command;

use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES};

/// `SE_GROUP_ENABLED`: the capability is active for the container.
const GROUP_ENABLED: u32 = 0x0000_0004;

use crate::policy::{Access, CompiledPolicy, Network};

use super::acl::{self, Sid};

/// `HRESULT` as every entry point below returns it.
type HResult = i32;

/// Well-known capability SIDs. `internetClient` is the outbound
/// capability; the other two add the listening side.
const CAPABILITY_INTERNET_CLIENT: &str = "S-1-15-3-1";
const CAPABILITY_INTERNET_CLIENT_SERVER: &str = "S-1-15-3-2";
const CAPABILITY_PRIVATE_NETWORK_CLIENT_SERVER: &str = "S-1-15-3-3";

/// Container name, stable across runs so a crashed run's grants are
/// findable by name rather than by search.
const CONTAINER_NAME: &str = "GossamerSandbox";

/// Whether `AppContainer` is usable on this host.
#[must_use]
pub(crate) fn is_available() -> bool {
    let wide = wide(CONTAINER_NAME);
    let mut sid = std::ptr::null_mut();
    let derived: HResult =
        unsafe { DeriveAppContainerSidFromAppContainerName(wide.as_ptr(), &raw mut sid) };
    if derived >= 0 && !sid.is_null() {
        unsafe { windows_sys::Win32::Foundation::LocalFree(sid.cast()) };
        return true;
    }
    false
}

/// A live container: its package SID, its profile, and every host ACL
/// grant it made.
pub(crate) struct Container {
    sid: Sid,
    record: acl::GrantRecord,
    granted: Vec<std::path::PathBuf>,
    capabilities: Box<SECURITY_CAPABILITIES>,
    /// Backing storage for the pointer in `capabilities`. Never read:
    /// holding it is what keeps that pointer valid until the process
    /// has been created.
    #[allow(dead_code, reason = "lifetime anchor for a raw pointer's target")]
    capability_sids: Vec<SID_AND_ATTRIBUTES>,
    /// `LocalFree` targets for the SIDs the array points at.
    owned_sids: Vec<*mut std::ffi::c_void>,
}

// Owned by this value; a SID is a plain allocation with no thread
// affinity.
#[allow(
    unsafe_code,
    reason = "a SID is a process-wide allocation with no thread affinity"
)]
unsafe impl Send for Container {}

impl Container {
    /// Creates the container profile and grants every path the policy
    /// allows.
    pub(crate) fn create(policy: &CompiledPolicy) -> Result<Self, String> {
        let name = wide(CONTAINER_NAME);
        let display = wide(CONTAINER_NAME);
        let description = wide("Gossamer sandboxed build");
        let created: HResult = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                display.as_ptr(),
                description.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
            )
        };
        // A profile that already exists is the normal case: the name is
        // deliberately stable.
        let already = created == hresult_from_win32(ERROR_ALREADY_EXISTS);
        if created < 0 && !already {
            return Err(format!("CreateAppContainerProfile failed: {created:#x}"));
        }

        let mut sid: Sid = std::ptr::null_mut();
        let derived: HResult =
            unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &raw mut sid) };
        if derived < 0 || sid.is_null() {
            return Err(format!(
                "DeriveAppContainerSidFromAppContainerName failed: {derived:#x}"
            ));
        }

        let mut record = acl::GrantRecord::open(CONTAINER_NAME);
        let mut granted = Vec::new();
        for rule in policy
            .rules
            .iter()
            .filter(|rule| rule.access != Access::Deny)
        {
            if !acl::is_owned_by_current_user(&rule.path) {
                return Err(format!(
                    "refusing to modify the ACL of {}, which this user does not own",
                    rule.path.display()
                ));
            }
            // Recorded before the grant, so a crash between the two
            // leaves a revoke that finds nothing rather than a grant
            // nothing knows about.
            record.add(&rule.path);
            acl::grant(&rule.path, sid, rule.access == Access::ReadWrite)?;
            granted.push(rule.path.clone());
        }

        // An AppContainer reaches the network only through a capability
        // SID, so the network policy is this list and nothing else.
        // Client is INTERNET_CLIENT; Open adds the server side.
        let mut capability_sids: Vec<SID_AND_ATTRIBUTES> = Vec::new();
        let mut owned_sids: Vec<*mut std::ffi::c_void> = Vec::new();
        let wanted: &[&str] = match policy.network {
            Network::None => &[],
            Network::Client => &[CAPABILITY_INTERNET_CLIENT],
            Network::Open => &[
                CAPABILITY_INTERNET_CLIENT,
                CAPABILITY_INTERNET_CLIENT_SERVER,
                CAPABILITY_PRIVATE_NETWORK_CLIENT_SERVER,
            ],
        };
        for text in wanted {
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let mut raw: *mut std::ffi::c_void = std::ptr::null_mut();
            // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that
            // outlives the call, and `raw` is written only on success.
            if unsafe { ConvertStringSidToSidW(wide.as_ptr(), &raw mut raw) } == 0 {
                for sid in &owned_sids {
                    unsafe { LocalFree(*sid) };
                }
                return Err(format!(
                    "converting capability SID {text}: {}",
                    std::io::Error::last_os_error()
                ));
            }
            owned_sids.push(raw);
            capability_sids.push(SID_AND_ATTRIBUTES {
                Sid: raw.cast(),
                Attributes: GROUP_ENABLED,
            });
        }
        let capabilities = Box::new(SECURITY_CAPABILITIES {
            AppContainerSid: sid.cast(),
            Capabilities: if capability_sids.is_empty() {
                std::ptr::null_mut()
            } else {
                capability_sids.as_mut_ptr()
            },
            CapabilityCount: u32::try_from(capability_sids.len()).unwrap_or(0),
            Reserved: 0,
        });
        Ok(Self {
            sid,
            record,
            granted,
            capabilities,
            capability_sids,
            owned_sids,
        })
    }

    /// The `SECURITY_CAPABILITIES` the process-creation attribute list
    /// takes.
    pub(crate) fn security_capabilities(&self) -> *mut std::ffi::c_void {
        std::ptr::from_ref(self.capabilities.as_ref())
            .cast::<std::ffi::c_void>()
            .cast_mut()
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        // Revoking is the inverse of granting and must happen even on
        // an unwinding path, which is why it lives in `Drop` rather
        // than at the end of the run.
        for path in &self.granted {
            let _ = acl::revoke(path, self.sid);
        }
        self.record.close();
        // Each capability SID was allocated by `ConvertStringSidToSid`
        // and is freed the same way the container SID is.
        for sid in &self.owned_sids {
            unsafe { LocalFree(*sid) };
        }
        unsafe { LocalFree(self.sid.cast()) };
    }
}

/// Applies the process mitigation policies the `standard` level uses.
///
/// These are creation flags rather than a token change, so they are the
/// part of the Windows story `std::process::Command` can express.
pub(crate) fn apply_standard_mitigations(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    /// `CREATE_NEW_PROCESS_GROUP`: the child does not receive the
    /// console's Ctrl+C, so the supervisor decides when the tree ends.
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    // `CREATE_BREAKAWAY_FROM_JOB` is deliberately not set: a child
    // that could leave the job would leave the tree teardown behind.
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

/// Revokes every grant an interrupted run left behind and removes its
/// record. Returns how many paths were revoked.
pub(crate) fn clean_stale_grants() -> Result<usize, String> {
    let name = wide(CONTAINER_NAME);
    let mut sid: Sid = std::ptr::null_mut();
    let derived: HResult =
        unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &raw mut sid) };
    if derived < 0 || sid.is_null() {
        return Err(format!(
            "DeriveAppContainerSidFromAppContainerName failed: {derived:#x}"
        ));
    }
    let mut revoked = 0usize;
    for (record, paths) in acl::stale_grants() {
        for path in paths {
            if acl::revoke(&path, sid).is_ok() {
                revoked += 1;
            }
        }
        let _ = std::fs::remove_file(record);
    }
    unsafe { LocalFree(sid.cast()) };
    Ok(revoked)
}

/// Removes the container profile, for `doctor --clean`.
pub(crate) fn delete_profile() -> Result<(), String> {
    let wide = wide(CONTAINER_NAME);
    let deleted: HResult = unsafe { DeleteAppContainerProfile(wide.as_ptr()) };
    if deleted < 0 {
        return Err(format!("DeleteAppContainerProfile failed: {deleted:#x}"));
    }
    Ok(())
}

/// `HRESULT_FROM_WIN32`, which the crate does not expose as a
/// function.
const fn hresult_from_win32(code: u32) -> HResult {
    if code == 0 {
        return 0;
    }
    ((code & 0x0000_FFFF) | 0x8007_0000) as HResult
}

/// A NUL-terminated UTF-16 copy of `text`, as every wide Win32 entry
/// point takes.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
