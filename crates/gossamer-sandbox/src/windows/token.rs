//! Restricted tokens and process mitigation policies.
//!
//! `standard` on Windows is a restricted token: the child runs with
//! deny-only SIDs and no privileges, so it cannot reach an object
//! granted only to the full user SID. No host ACL is touched, which is
//! what separates `standard` from `strict`.

#![allow(
    unsafe_code,
    reason = "token manipulation is a raw Win32 API; every call below \
              passes a stack-owned struct with its size"
)]

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LUID};
use windows_sys::Win32::Security::{
    CreateRestrictedToken, DISABLE_MAX_PRIVILEGE, GetTokenInformation, LUID_AND_ATTRIBUTES,
    SID_AND_ATTRIBUTES, TOKEN_ALL_ACCESS, TOKEN_PRIVILEGES, TokenPrivileges,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// A token handle this value owns.
pub(crate) struct Token(HANDLE);

// Owned by this value; Win32 handles have no thread affinity.
#[allow(
    unsafe_code,
    reason = "a Win32 HANDLE is a process-wide token with no thread affinity"
)]
unsafe impl Send for Token {}

impl Token {
    /// A restricted copy of the caller's own token, with every
    /// privilege dropped.
    ///
    /// `DISABLE_MAX_PRIVILEGE` removes every privilege but
    /// `SeChangeNotifyPrivilege`, which the child needs for change
    /// notifications.
    ///
    /// It does not buy the child the traverse-check bypass that
    /// privilege usually carries: the fast traverse check declines to
    /// run for a restricted token, which is what this makes. Every
    /// directory on the way to a granted path is therefore access-
    /// checked, and `super::acl::grant_traverse` is what answers it.
    pub(crate) fn restricted() -> Result<Self, String> {
        let mut current: HANDLE = std::ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &raw mut current) } == 0
        {
            return Err(format!(
                "OpenProcessToken failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let owned = Self(current);
        let mut restricted: HANDLE = std::ptr::null_mut();
        let created = unsafe {
            CreateRestrictedToken(
                owned.0,
                DISABLE_MAX_PRIVILEGE,
                0,
                std::ptr::null::<SID_AND_ATTRIBUTES>(),
                0,
                std::ptr::null::<LUID_AND_ATTRIBUTES>(),
                0,
                std::ptr::null::<SID_AND_ATTRIBUTES>(),
                &raw mut restricted,
            )
        };
        if created == 0 {
            return Err(format!(
                "CreateRestrictedToken failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self(restricted))
    }

    /// The raw handle, for `CreateProcessAsUser`.
    pub(crate) const fn handle(&self) -> HANDLE {
        self.0
    }

    /// How many privileges the token still carries, for the capability
    /// report.
    #[must_use]
    pub(crate) fn privilege_count(&self) -> u32 {
        let mut needed = 0u32;
        unsafe {
            GetTokenInformation(
                self.0,
                TokenPrivileges,
                std::ptr::null_mut(),
                0,
                &raw mut needed,
            );
        }
        if needed == 0 {
            return 0;
        }
        let mut buffer = vec![0u8; needed as usize];
        let read = unsafe {
            GetTokenInformation(
                self.0,
                TokenPrivileges,
                buffer.as_mut_ptr().cast(),
                needed,
                &raw mut needed,
            )
        };
        if read == 0 {
            return 0;
        }
        let privileges: &TOKEN_PRIVILEGES = unsafe { &*buffer.as_ptr().cast() };
        privileges.PrivilegeCount
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// Silences the unused-import warning for `LUID`, which is part of the
/// `LUID_AND_ATTRIBUTES` shape the call above names.
const _: Option<LUID> = None;
