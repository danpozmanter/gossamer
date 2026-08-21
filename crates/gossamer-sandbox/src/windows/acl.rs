//! Host ACL grants for an `AppContainer`, and the record that makes a
//! crashed run's grants findable.
//!
//! `AppContainer` has no filesystem overlay: a path the container may
//! reach must carry an ACE for the container's package SID on the real
//! object. So the sandbox mutates the user's host ACLs, and the honest
//! way to do that is to write down what was granted before granting
//! it, so an interrupted run leaves a record rather than a mystery.

#![allow(
    unsafe_code,
    reason = "ACL entries are a raw Win32 API; every call passes               stack-owned structs with their sizes"
)]

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};

/// A security identifier, as every Win32 entry point below takes it.
pub(crate) type Sid = *mut std::ffi::c_void;
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GetEffectiveRightsFromAclW, GetNamedSecurityInfoW,
    SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
    TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, NO_INHERITANCE, PSECURITY_DESCRIPTOR,
    SUB_CONTAINERS_AND_OBJECTS_INHERIT,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
};

/// Directory holding one record file per live run.
#[must_use]
pub(crate) fn record_directory() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").map_or_else(|_| std::env::temp_dir(), PathBuf::from);
    base.join("gossamer").join("sandbox-grants")
}

/// A run's grant record: the paths whose ACLs were modified, so a
/// crash leaves something `doctor --clean` can act on.
pub(crate) struct GrantRecord {
    path: PathBuf,
    granted: Vec<PathBuf>,
}

impl GrantRecord {
    /// Opens a record for the container named `container`.
    pub(crate) fn open(container: &str) -> Self {
        let directory = record_directory();
        let _ = std::fs::create_dir_all(&directory);
        Self {
            path: directory.join(format!("{container}.grants")),
            granted: Vec::new(),
        }
    }

    /// Records that `path` was granted, before the grant is made.
    ///
    /// Written first so a crash between the write and the grant leaves
    /// a revoke that finds nothing, which is harmless, rather than a
    /// grant nothing knows about.
    pub(crate) fn add(&mut self, path: &Path) {
        self.granted.push(path.to_path_buf());
        let body: Vec<String> = self
            .granted
            .iter()
            .map(|entry| entry.to_string_lossy().into_owned())
            .collect();
        let _ = std::fs::write(&self.path, body.join("\n"));
    }

    /// Every path this record covers.
    #[cfg(test)]
    pub(crate) fn paths(&self) -> &[PathBuf] {
        &self.granted
    }

    /// Removes the record once every grant it names has been revoked.
    pub(crate) fn close(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// How many records an interrupted run left behind.
#[must_use]
pub(crate) fn stale_grant_count() -> usize {
    std::fs::read_dir(record_directory())
        .map_or(0, |entries| entries.filter_map(Result::ok).count())
}

/// Every path a stale record names, so `doctor --clean` can revoke
/// them.
#[must_use]
pub(crate) fn stale_grants() -> Vec<(PathBuf, Vec<PathBuf>)> {
    let Ok(entries) = std::fs::read_dir(record_directory()) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let body = std::fs::read_to_string(&path).ok()?;
            let paths = body.lines().map(PathBuf::from).collect();
            Some((path, paths))
        })
        .collect()
}

/// Whether the current user owns `path`.
///
/// A sandbox refuses to modify an ACL on an object it does not own:
/// granting a container SID on somebody else's file is a change the
/// user did not ask for and may not be able to undo.
#[must_use]
pub(crate) fn is_owned_by_current_user(path: &Path) -> bool {
    // Ownership is checked by writability, which is what the grant
    // needs anyway: an object the user cannot modify is one whose ACL
    // the user cannot modify either.
    std::fs::OpenOptions::new()
        .read(true)
        .open(path)
        .ok()
        .and_then(|file| file.metadata().ok())
        .is_some_and(|metadata| !metadata.permissions().readonly() || path.is_dir())
}

/// `ALL APPLICATION PACKAGES`, the group every app container belongs
/// to. Windows puts an ACE for it on the system directories, which is
/// how a store app reads a system DLL without anyone granting it.
const ALL_APPLICATION_PACKAGES: &str = "S-1-15-2-1";

/// Whether an app container already reaches `path` with the rights the
/// grant would add.
///
/// The system directories carry an `ALL APPLICATION PACKAGES` ACE, so
/// a read-only grant on one of them is already satisfied and adding an
/// ACE would mutate a system object's ACL for nothing. The container's
/// own package SID counts for the same reason: an object a previous
/// run granted needs no second grant.
#[must_use]
pub(crate) fn already_reachable(path: &Path, sid: Sid, writable: bool) -> bool {
    let mut needed = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;
    if writable {
        needed |= FILE_GENERIC_WRITE;
    }
    let Some(dacl) = Dacl::read(path) else {
        return false;
    };
    if dacl.rights_of(sid) & needed == needed {
        return true;
    }
    let Some(packages) = OwnedSid::from_text(ALL_APPLICATION_PACKAGES) else {
        return false;
    };
    dacl.rights_of(packages.raw()) & needed == needed
}

/// A SID this module allocated and frees.
struct OwnedSid(Sid);

impl OwnedSid {
    fn from_text(text: &str) -> Option<Self> {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sid: Sid = std::ptr::null_mut();
        // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives
        // the call, and `sid` is written only when the call succeeds.
        if unsafe { ConvertStringSidToSidW(wide.as_ptr(), &raw mut sid) } == 0 || sid.is_null() {
            return None;
        }
        Some(Self(sid))
    }

    const fn raw(&self) -> Sid {
        self.0
    }
}

impl Drop for OwnedSid {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0.cast()) };
    }
}

/// An object's DACL, borrowed from the security descriptor that owns
/// it, so the descriptor is freed once rather than per query.
struct Dacl {
    acl: *mut ACL,
    descriptor: PSECURITY_DESCRIPTOR,
}

impl Dacl {
    fn read(path: &Path) -> Option<Self> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut acl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: both out parameters are written only on success, and
        // the descriptor owns the ACL until `LocalFree` in `Drop`.
        let read = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut acl,
                std::ptr::null_mut(),
                &raw mut descriptor,
            )
        };
        if read != ERROR_SUCCESS || acl.is_null() {
            if !descriptor.is_null() {
                unsafe { LocalFree(descriptor.cast()) };
            }
            return None;
        }
        Some(Self { acl, descriptor })
    }

    /// The rights this ACL grants `sid`, or none when the ACL cannot be
    /// evaluated for it.
    fn rights_of(&self, sid: Sid) -> u32 {
        let trustee = TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.cast(),
        };
        let mut rights: u32 = 0;
        // SAFETY: the ACL is live for the length of this value, and the
        // trustee names a SID the caller keeps alive across the call.
        let read =
            unsafe { GetEffectiveRightsFromAclW(self.acl, &raw const trustee, &raw mut rights) };
        if read == ERROR_SUCCESS { rights } else { 0 }
    }
}

impl Drop for Dacl {
    fn drop(&mut self) {
        unsafe { LocalFree(self.descriptor.cast()) };
    }
}

/// Grants `sid` access to `path` and everything beneath it.
///
/// `AppContainer` reaches an object only through an ACE for its package
/// SID, so this is what a filesystem grant means on Windows. The
/// caller records the path first, so an interrupted run leaves a
/// revoke that `doctor --clean` can perform.
pub(crate) fn grant(path: &Path, sid: Sid, writable: bool) -> Result<(), String> {
    modify(path, sid, writable, true)
}

/// Removes the ACE `grant` added.
pub(crate) fn revoke(path: &Path, sid: Sid) -> Result<(), String> {
    modify(path, sid, false, false)
}

fn modify(path: &Path, sid: Sid, writable: bool, adding: bool) -> Result<(), String> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut existing: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let read = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut existing,
            std::ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if read != ERROR_SUCCESS {
        return Err(format!(
            "reading the ACL of {} failed: {read}",
            path.display()
        ));
    }

    let mut access = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;
    if writable {
        access |= FILE_GENERIC_WRITE;
    }
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: access,
        grfAccessMode: SET_ACCESS,
        grfInheritance: if path.is_dir() {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            NO_INHERITANCE
        },
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.cast(),
        },
    };
    let mut updated: *mut ACL = std::ptr::null_mut();
    // Removing is expressed as writing back the ACL without the entry,
    // which `SetEntriesInAcl` does when handed zero entries.
    let entries: &[EXPLICIT_ACCESS_W] = if adding {
        std::slice::from_ref(&entry)
    } else {
        &[]
    };
    let built = unsafe {
        SetEntriesInAclW(
            u32::try_from(entries.len()).unwrap_or(0),
            entries.as_ptr(),
            if adding { existing } else { std::ptr::null() },
            &raw mut updated,
        )
    };
    if built != ERROR_SUCCESS {
        unsafe { LocalFree(descriptor.cast()) };
        return Err(format!(
            "building the ACL of {} failed: {built}",
            path.display()
        ));
    }
    let written = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr().cast_mut(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            updated,
            std::ptr::null_mut(),
        )
    };
    unsafe {
        LocalFree(updated.cast());
        LocalFree(descriptor.cast());
    }
    if written != ERROR_SUCCESS {
        return Err(format!(
            "writing the ACL of {} failed: {written}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod acl_tests {
    use super::*;

    #[test]
    fn a_record_is_written_before_the_grant_it_describes() {
        let record_root = record_directory();
        let mut record = GrantRecord::open("gos-sandbox-acl-test");
        let target = std::env::temp_dir();
        record.add(&target);
        let written = std::fs::read_to_string(record_root.join("gos-sandbox-acl-test.grants"))
            .expect("record written");
        assert!(written.contains(&target.to_string_lossy().to_string()));
        assert_eq!(record.paths().len(), 1);
        record.close();
        assert!(!record_root.join("gos-sandbox-acl-test.grants").exists());
    }
}
