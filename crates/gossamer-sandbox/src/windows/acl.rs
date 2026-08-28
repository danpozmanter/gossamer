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

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, INVALID_HANDLE_VALUE, LocalFree};

/// A security identifier, as every Win32 entry point below takes it.
pub(crate) type Sid = *mut std::ffi::c_void;
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW, DENY_ACCESS,
    EXPLICIT_ACCESS_W, GetNamedSecurityInfoW, REVOKE_ACCESS, SDDL_REVISION_1, SE_FILE_OBJECT,
    SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN,
    TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
    GetSecurityDescriptorSacl, LABEL_SECURITY_INFORMATION, NO_INHERITANCE, PSECURITY_DESCRIPTOR,
    SUB_CONTAINERS_AND_OBJECTS_INHERIT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, OPEN_EXISTING, READ_CONTROL, SYNCHRONIZE,
    WRITE_DAC, WRITE_OWNER,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Directory holding one record file per live run.
#[must_use]
pub(crate) fn record_directory() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").map_or_else(|_| std::env::temp_dir(), PathBuf::from);
    base.join("gossamer").join("sandbox-grants")
}

/// What a run did to one host object, so the inverse is knowable from
/// the record alone.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Edit {
    /// An allow or deny ACE for the container's package SID. Undone by
    /// removing every entry that SID has on the object.
    Ace,
    /// The object's mandatory label was lowered so the container, which
    /// runs at low integrity, could write to it. Undone by putting the
    /// previous label back - held here as the SDDL the object carried,
    /// or empty when it carried none.
    Label(String),
}

impl Edit {
    /// The record line's leading tag.
    fn tag(&self) -> &'static str {
        match self {
            Self::Ace => "ace",
            Self::Label(_) => "label",
        }
    }
}

/// A run's grant record: the host objects whose security was modified,
/// so a crash leaves something the next run can undo.
pub(crate) struct GrantRecord {
    path: PathBuf,
    edits: Vec<(Edit, PathBuf)>,
}

impl GrantRecord {
    /// Opens a record for the container named `container`.
    ///
    /// The name carries this process's id, so two runs never share a
    /// record and a sweep can tell a record whose run is still going
    /// from one an interrupted run left behind.
    pub(crate) fn open(container: &str) -> Self {
        let directory = record_directory();
        let _ = std::fs::create_dir_all(&directory);
        Self {
            path: directory.join(format!("{container}.{}.grants", std::process::id())),
            edits: Vec::new(),
        }
    }

    /// Records that `path` is about to be edited, before the edit is
    /// made.
    ///
    /// Written first so a crash between the write and the edit leaves an
    /// undo that finds nothing, which is harmless, rather than an edit
    /// nothing knows about.
    pub(crate) fn add(&mut self, edit: Edit, path: &Path) {
        self.edits.push((edit, path.to_path_buf()));
        let body: Vec<String> = self
            .edits
            .iter()
            .map(|(edit, entry)| {
                let detail = match edit {
                    Edit::Ace => String::new(),
                    Edit::Label(previous) => previous.clone(),
                };
                format!("{}\t{}\t{detail}", edit.tag(), entry.to_string_lossy())
            })
            .collect();
        let _ = std::fs::write(&self.path, body.join("\n"));
    }

    /// Every edit this record covers.
    #[cfg(test)]
    pub(crate) fn edits(&self) -> &[(Edit, PathBuf)] {
        &self.edits
    }

    /// The file this record is written to.
    #[cfg(test)]
    pub(crate) fn file(&self) -> &Path {
        &self.path
    }

    /// Removes the record once every edit it names has been undone.
    pub(crate) fn close(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// One record line as the edit it describes.
///
/// A line with no tab is a path written by an older toolchain, whose
/// records only ever named ACE grants.
fn parse_record_line(line: &str) -> Option<(Edit, PathBuf)> {
    if line.is_empty() {
        return None;
    }
    let mut fields = line.split('\t');
    let first = fields.next()?;
    let Some(path) = fields.next() else {
        return Some((Edit::Ace, PathBuf::from(first)));
    };
    let detail = fields.next().unwrap_or_default();
    let edit = match first {
        "label" => Edit::Label(detail.to_string()),
        _ => Edit::Ace,
    };
    Some((edit, PathBuf::from(path)))
}

/// Every path a record left by a run that is no longer running names.
///
/// A record whose process is still alive belongs to a concurrent run,
/// and revoking its grants would pull the ground out from under a
/// sandbox that is still using them.
#[must_use]
pub(crate) fn stale_grants() -> Vec<(PathBuf, Vec<(Edit, PathBuf)>)> {
    let Ok(entries) = std::fs::read_dir(record_directory()) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| !owner_is_running(&entry.path()))
        .filter_map(|entry| {
            let path = entry.path();
            let body = std::fs::read_to_string(&path).ok()?;
            let edits = body.lines().filter_map(parse_record_line).collect();
            Some((path, edits))
        })
        .collect()
}

/// Whether the run that wrote `record` is still running.
///
/// The record's name carries the writing process's id. A pid the
/// system no longer knows is a run that ended; a pid it does know is
/// treated as live, which at worst leaves a stale record for the next
/// sweep rather than revoking a live run's grants.
fn owner_is_running(record: &Path) -> bool {
    let Some(pid) = record
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.rsplit('.').next())
        .and_then(|digits| digits.parse::<u32>().ok())
    else {
        // A record from an older naming scheme names no process, so
        // nothing claims it.
        return false;
    };
    if pid == std::process::id() {
        return true;
    }
    // SAFETY: `OpenProcess` takes no pointer; the handle is closed
    // below on every path that opened one.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let mut code: u32 = 0;
    let read = unsafe { GetExitCodeProcess(handle, &raw mut code) };
    unsafe { CloseHandle(handle) };
    read != 0 && code == STILL_ACTIVE_CODE
}

/// `STILL_ACTIVE`, the exit code a running process reports.
const STILL_ACTIVE_CODE: u32 = 259;

/// Whether the current user may rewrite the ACL of `path`.
///
/// A sandbox refuses to modify an ACL on an object it does not own:
/// granting a container SID on somebody else's file is a change the
/// user did not ask for and may not be able to undo.
///
/// The question is asked of the object itself, by opening it for
/// `WRITE_DAC`: an owner always holds that right, and anyone else holds
/// it only where the DACL says so, which is exactly the permission the
/// grant needs. `FILE_FLAG_BACKUP_SEMANTICS` is what lets a directory
/// be opened at all, and nearly every path a policy grants is one.
#[must_use]
pub(crate) fn is_owned_by_current_user(path: &Path) -> bool {
    can_open_with(path, WRITE_DAC)
}

/// Whether `path` opens for `access`.
///
/// `FILE_FLAG_BACKUP_SEMANTICS` is what lets a directory be opened at
/// all, and nearly every path a policy names is one.
fn can_open_with(path: &Path, access: u32) -> bool {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the
    // call, and the handle is closed on every path out.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return false;
    }
    unsafe { CloseHandle(handle) };
    true
}

/// `ALL APPLICATION PACKAGES`, the group every app container belongs
/// to. Windows puts an ACE for it on the system directories, which is
/// how a store app reads a system DLL without anyone granting it.
const ALL_APPLICATION_PACKAGES: &str = "S-1-15-2-1";

/// `ACCESS_ALLOWED_ACE_TYPE` and `ACCESS_DENIED_ACE_TYPE`, which the
/// crate declares the structures for but not the tags.
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;

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
///
/// `acl` is null for an object with no DACL at all, which Windows reads
/// as "everyone has every right".
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
        if read != ERROR_SUCCESS {
            if !descriptor.is_null() {
                unsafe { LocalFree(descriptor.cast()) };
            }
            return None;
        }
        Some(Self { acl, descriptor })
    }

    /// The rights this ACL grants `sid` by name.
    ///
    /// The ACEs are walked rather than handed to
    /// `GetEffectiveRightsFromAcl`, which enumerates a group trustee's
    /// members - impossible for a well-known group like
    /// `ALL APPLICATION PACKAGES` - and refuses outright any ACL that
    /// carries an inherited deny ACE, which a system directory's does.
    fn rights_of(&self, sid: Sid) -> u32 {
        if self.acl.is_null() {
            return u32::MAX;
        }
        // SAFETY: the descriptor this ACL belongs to is live for the
        // length of this value, so the header is readable.
        let count = u32::from(unsafe { (*self.acl).AceCount });
        let mut granted: u32 = 0;
        let mut denied: u32 = 0;
        for index in 0..count {
            let mut entry: *mut std::ffi::c_void = std::ptr::null_mut();
            // SAFETY: `index` is below the header's own ACE count, and
            // the entry is written only when the call succeeds.
            if unsafe { GetAce(self.acl, index, &raw mut entry) } == 0 || entry.is_null() {
                continue;
            }
            // SAFETY: every ACE begins with an `ACE_HEADER`, and an
            // allow or deny ACE continues with a mask and a SID. The
            // pointer came from `GetAce` on this ACL.
            #[allow(clippy::cast_ptr_alignment)]
            let header = unsafe { &*entry.cast::<ACE_HEADER>() };
            if header.AceType != ACCESS_ALLOWED_ACE_TYPE && header.AceType != ACCESS_DENIED_ACE_TYPE
            {
                continue;
            }
            // A deny ACE has the same prefix layout as an allow ACE.
            #[allow(clippy::cast_ptr_alignment)]
            let ace = unsafe { &*entry.cast::<ACCESS_ALLOWED_ACE>() };
            let ace_sid: Sid = std::ptr::from_ref(&ace.SidStart).cast_mut().cast();
            // SAFETY: both SIDs are live; the ACE's belongs to the ACL.
            if unsafe { EqualSid(ace_sid, sid) } == 0 {
                continue;
            }
            if header.AceType == ACCESS_ALLOWED_ACE_TYPE {
                granted |= ace.Mask;
            } else {
                denied |= ace.Mask;
            }
        }
        granted & !denied
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
    modify(path, sid, writable, Mode::Allow)
}

/// The rights an ancestor directory has to give up for the container to
/// reach what lies beneath it.
///
/// Opening `dir\\file` is an access check on `dir` as well as on `file`.
/// An ordinary token skips those with `SeChangeNotifyPrivilege`, but the
/// fast traverse check declines to run for a restricted token, and the
/// child's token is restricted twice over: `CreateRestrictedToken` makes
/// it so, and the app container makes it a lowbox. So every directory on
/// the way to a granted object is checked, and each has to name the
/// package SID.
///
/// This is the narrow spelling on purpose: it walks into the directory
/// and reads its entries, and it says nothing about creating, deleting,
/// or writing anything there.
const TRAVERSE_RIGHTS: u32 =
    FILE_TRAVERSE | FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE;

/// Every directory between the volume root and `path`, nearest first,
/// excluding `path` itself.
///
/// A relative path has no ancestor chain to walk, so it contributes
/// none: a policy compiled from one is not reachable by absolute path
/// either, and the grant would be guesswork.
#[must_use]
pub(crate) fn ancestors_of(path: &Path) -> Vec<PathBuf> {
    if !path.is_absolute() {
        return Vec::new();
    }
    path.ancestors()
        .skip(1)
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .collect()
}

/// Lets `sid` walk through `path` on the way to something beneath it,
/// and nothing more.
///
/// `NO_INHERITANCE`, so the directory's children gain nothing: only the
/// named directory becomes walkable, and its siblings and contents are
/// untouched.
pub(crate) fn grant_traverse(path: &Path, sid: Sid) -> Result<(), String> {
    modify_with(path, sid, TRAVERSE_RIGHTS, Mode::Allow, NO_INHERITANCE)
}

/// Whether `sid` can already walk through `path`.
///
/// The system directories carry an `ALL APPLICATION PACKAGES` ACE, which
/// is why an app container reaches `C:\\Windows\\system32\\cmd.exe` with
/// nobody granting it anything, and why this sandbox must not rewrite
/// their ACLs to say what they already say.
#[must_use]
pub(crate) fn already_traversable(path: &Path, sid: Sid) -> bool {
    let Some(dacl) = Dacl::read(path) else {
        return false;
    };
    if dacl.rights_of(sid) & TRAVERSE_RIGHTS == TRAVERSE_RIGHTS {
        return true;
    }
    let Some(packages) = OwnedSid::from_text(ALL_APPLICATION_PACKAGES) else {
        return false;
    };
    dacl.rights_of(packages.raw()) & TRAVERSE_RIGHTS == TRAVERSE_RIGHTS
}

/// The SDDL for a low mandatory label that a subtree inherits.
///
/// `ML` is a mandatory-label ACE, `OICI` makes files and directories
/// beneath it inherit the label, `NW` is the no-write-up policy every
/// label carries, and `LW` is the low integrity level.
const LOW_LABEL_SDDL: &str = "S:(ML;OICI;NW;;;LW)";

/// An object's mandatory label as SDDL, or an empty string when it
/// carries none.
///
/// An object with no label is treated by Windows as medium integrity,
/// which is why the empty string is a real answer and not a failure.
#[must_use]
pub(crate) fn integrity_label(path: &Path) -> String {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the path outlives the call and the descriptor is written
    // only on success, then freed on every path out.
    let read = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if read != ERROR_SUCCESS || descriptor.is_null() {
        return String::new();
    }
    let mut sacl: *mut ACL = std::ptr::null_mut();
    let mut present = 0i32;
    let mut defaulted = 0i32;
    // SAFETY: the descriptor is live and every out parameter is a
    // stack slot of the declared type.
    let got = unsafe {
        GetSecurityDescriptorSacl(
            descriptor,
            &raw mut present,
            &raw mut sacl,
            &raw mut defaulted,
        )
    };
    let labelled = got != 0 && present != 0 && !sacl.is_null();
    let text = if labelled {
        descriptor_to_sddl(descriptor)
    } else {
        String::new()
    };
    unsafe { LocalFree(descriptor.cast()) };
    text
}

/// The `LABEL_SECURITY_INFORMATION` part of `descriptor` as SDDL.
fn descriptor_to_sddl(descriptor: PSECURITY_DESCRIPTOR) -> String {
    let mut text: *mut u16 = std::ptr::null_mut();
    let mut length: u32 = 0;
    // SAFETY: the descriptor is live for the call, and `text` is written
    // only on success and freed below.
    let converted = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            LABEL_SECURITY_INFORMATION,
            &raw mut text,
            &raw mut length,
        )
    };
    if converted == 0 || text.is_null() {
        return String::new();
    }
    let mut out = Vec::new();
    let mut cursor = text;
    // SAFETY: the buffer the converter returned is NUL-terminated.
    unsafe {
        while *cursor != 0 {
            out.push(*cursor);
            cursor = cursor.add(1);
        }
        LocalFree(text.cast());
    }
    String::from_utf16_lossy(&out)
}

/// Lowers `path`'s mandatory label to low, so the container may write
/// there, and answers the label it carried before.
///
/// An app container process runs at low integrity, and integrity is
/// checked before the DACL is consulted: a low process writing to an
/// object Windows treats as medium is refused whatever the DACL says.
/// So a writable grant is two edits, not one, and this is the second.
/// The caller records the returned label first, because putting it back
/// is the only way to undo this.
pub(crate) fn set_low_integrity(path: &Path) -> Result<(), String> {
    write_label(path, LOW_LABEL_SDDL)
}

/// Puts back the label an object carried before [`set_low_integrity`],
/// or removes the label entirely when it carried none.
pub(crate) fn restore_integrity(path: &Path, previous: &str) -> Result<(), String> {
    if previous.is_empty() {
        // An empty SACL in the label channel is how a label is removed:
        // the object goes back to being unlabelled, which Windows reads
        // as medium integrity.
        return write_label(path, "S:");
    }
    write_label(path, previous)
}

/// Writes the label channel of `path`'s security from `sddl`.
fn write_label(path: &Path, sddl: &str) -> Result<(), String> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let text: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `text` is a NUL-terminated UTF-16 buffer that outlives the
    // call; the descriptor is written only on success and freed below.
    let built = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if built == 0 || descriptor.is_null() {
        return Err(format!(
            "building the mandatory label for {} failed: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    let mut sacl: *mut ACL = std::ptr::null_mut();
    let mut present = 0i32;
    let mut defaulted = 0i32;
    // SAFETY: the descriptor came from the converter above and is live.
    let got = unsafe {
        GetSecurityDescriptorSacl(
            descriptor,
            &raw mut present,
            &raw mut sacl,
            &raw mut defaulted,
        )
    };
    if got == 0 {
        unsafe { LocalFree(descriptor.cast()) };
        return Err(format!(
            "reading the built mandatory label for {} failed: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: the SACL is borrowed from the descriptor, which is freed
    // only after the write returns.
    let written = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr().cast_mut(),
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            sacl,
        )
    };
    unsafe { LocalFree(descriptor.cast()) };
    if written != ERROR_SUCCESS {
        return Err(format!(
            "writing the mandatory label of {} failed: {written}",
            path.display()
        ));
    }
    Ok(())
}

/// Whether the current user may rewrite the mandatory label of `path`.
///
/// The label lives in the SACL, which `WRITE_OWNER` governs rather than
/// `WRITE_DAC`; an owner holds it, and nobody else does without being
/// told to.
#[must_use]
pub(crate) fn label_is_writable_by_current_user(path: &Path) -> bool {
    can_open_with(path, WRITE_OWNER)
}

/// Refuses `sid` every right on `path` and everything beneath it.
///
/// A grant of a parent directory reaches its children by inheritance,
/// so a denial inside a granted tree needs an entry of its own. Windows
/// evaluates a deny ACE ahead of an inherited allow, which is the same
/// verdict the other backends reach by granting the denial's siblings
/// instead.
pub(crate) fn deny(path: &Path, sid: Sid) -> Result<(), String> {
    modify(path, sid, false, Mode::Deny)
}

/// Removes every ACE `grant` or `deny` added for `sid`, and nothing
/// else.
pub(crate) fn revoke(path: &Path, sid: Sid) -> Result<(), String> {
    modify(path, sid, false, Mode::Revoke)
}

/// Which entry [`modify`] writes for the trustee.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Add an allow entry.
    Allow,
    /// Add a deny entry, which Windows evaluates first.
    Deny,
    /// Remove every entry the trustee has, of either kind.
    Revoke,
}

/// Edits the DACL of `path` as it stands: adds one entry for `sid`, or
/// removes the entries for `sid`.
///
/// Both directions hand `SetEntriesInAcl` the existing ACL, so every
/// entry that is not the sandbox's - the owner's own access above all -
/// is written back exactly as it was read. A revoke is never a
/// replacement: an ACL rebuilt from nothing is an empty DACL, which
/// denies everyone, and a directory's children then inherit that
/// nothing.
fn modify(path: &Path, sid: Sid, writable: bool, mode: Mode) -> Result<(), String> {
    let mut access = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;
    if writable || mode == Mode::Deny {
        access |= FILE_GENERIC_WRITE;
    }
    let inheritance = if path.is_dir() {
        SUB_CONTAINERS_AND_OBJECTS_INHERIT
    } else {
        NO_INHERITANCE
    };
    modify_with(path, sid, access, mode, inheritance)
}

/// [`modify`] with the rights and inheritance spelled out, for an edit
/// whose shape is not "everything the policy allows on this object".
fn modify_with(
    path: &Path,
    sid: Sid,
    access: u32,
    mode: Mode,
    inheritance: u32,
) -> Result<(), String> {
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
    // No DACL at all grants everyone every right, so the container
    // already reaches the object and there is no entry of ours to
    // remove. Writing it back as a list would turn "everyone" into
    // whatever the list says.
    if existing.is_null() {
        unsafe { LocalFree(descriptor.cast()) };
        return Ok(());
    }

    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: access,
        grfAccessMode: match mode {
            Mode::Allow => SET_ACCESS,
            Mode::Deny => DENY_ACCESS,
            Mode::Revoke => REVOKE_ACCESS,
        },
        grfInheritance: inheritance,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.cast(),
        },
    };
    let mut updated: *mut ACL = std::ptr::null_mut();
    // SAFETY: `entry` and the SID it names outlive the call, `existing`
    // is owned by `descriptor` which is freed below, and `updated` is
    // written only on success.
    let built = unsafe { SetEntriesInAclW(1, &raw const entry, existing, &raw mut updated) };
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

    fn rights_of(path: &Path, sid: Sid) -> u32 {
        Dacl::read(path).map_or(0, |dacl| dacl.rights_of(sid))
    }

    /// The revoke is the inverse of the grant and nothing more: the
    /// sandbox's entry goes, and every entry the object had before -
    /// its owner's own access above all - is still there, on the
    /// directory and on what the directory's grant was inherited by.
    #[test]
    fn a_revoke_removes_only_the_entry_the_grant_added() {
        let root = std::env::temp_dir().join("gos-sandbox-acl-revoke");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create root");
        let child = root.join("child.txt");
        std::fs::write(&child, "before").expect("write child");
        // A package-shaped SID no profile owns: the ACL edit is the
        // same whoever the trustee is.
        let sid = OwnedSid::from_text("S-1-15-2-1111-2222-3333-4444-5555-6666-7777-8888")
            .expect("a well-formed SID converts");
        let needed = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;
        assert_eq!(rights_of(&root, sid.raw()) & needed, 0);

        grant(&root, sid.raw(), false).expect("grant");
        assert_eq!(rights_of(&root, sid.raw()) & needed, needed);
        assert_eq!(
            rights_of(&child, sid.raw()) & needed,
            needed,
            "a directory grant reaches the files beneath it"
        );

        revoke(&root, sid.raw()).expect("revoke");
        assert_eq!(rights_of(&root, sid.raw()) & needed, 0);
        assert_eq!(rights_of(&child, sid.raw()) & needed, 0);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&child)
            .expect("the file is still writable by its owner once the grant is gone");
        std::fs::write(root.join("after.txt"), "after")
            .expect("the directory still takes a new file once the grant is gone");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_record_is_written_before_the_grant_it_describes() {
        let mut record = GrantRecord::open("gos-sandbox-acl-test");
        let file = record.file().to_path_buf();
        assert_eq!(file.parent(), Some(record_directory().as_path()));
        // The name carries the writing run's pid, which is what lets a
        // sweep tell a live run's record from one left behind.
        let name = file
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .expect("a record file has a name");
        assert!(name.contains(&std::process::id().to_string()));
        let target = std::env::temp_dir();
        record.add(Edit::Ace, &target);
        let written = std::fs::read_to_string(&file).expect("record written");
        assert!(written.contains(&target.to_string_lossy().to_string()));
        assert_eq!(record.edits().len(), 1);
        record.close();
        assert!(!file.exists());
    }

    /// A record has to say what kind of edit it describes: an ACE and a
    /// lowered mandatory label have different inverses, and a sweep
    /// after a crash has only the record to go on.
    #[test]
    fn a_record_line_carries_the_edit_and_the_label_it_replaced() {
        let mut record = GrantRecord::open("gos-sandbox-acl-kinds");
        let file = record.file().to_path_buf();
        let target = std::env::temp_dir();
        record.add(Edit::Ace, &target);
        record.add(Edit::Label("S:(ML;OICI;NW;;;ME)".to_string()), &target);
        let written = std::fs::read_to_string(&file).expect("record written");
        let parsed: Vec<(Edit, PathBuf)> = written.lines().filter_map(parse_record_line).collect();
        assert_eq!(
            parsed,
            vec![
                (Edit::Ace, target.clone()),
                (Edit::Label("S:(ML;OICI;NW;;;ME)".to_string()), target),
            ]
        );
        record.close();
    }

    /// A record written by an older toolchain names a path and nothing
    /// else, and every one of those was an ACE grant.
    #[test]
    fn a_bare_path_line_reads_as_an_ace() {
        assert_eq!(
            parse_record_line("C:\\some\\path"),
            Some((Edit::Ace, PathBuf::from("C:\\some\\path")))
        );
    }

    /// The ancestors are what the container has to walk through, so the
    /// granted path itself is not among them and a relative path
    /// contributes none.
    #[test]
    fn the_ancestors_of_a_path_stop_short_of_the_path_itself() {
        let ancestors = ancestors_of(Path::new("C:\\a\\b\\c"));
        assert_eq!(
            ancestors,
            vec![
                PathBuf::from("C:\\a\\b"),
                PathBuf::from("C:\\a"),
                PathBuf::from("C:\\"),
            ]
        );
        assert!(ancestors_of(Path::new("b\\c")).is_empty());
    }

    /// A traverse grant lets the container walk in and read the
    /// directory's entries, and gives it no way to change anything.
    #[test]
    fn a_traverse_grant_adds_no_write_right() {
        let root = std::env::temp_dir().join("gos-sandbox-acl-traverse");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create root");
        let sid = OwnedSid::from_text("S-1-15-2-1111-2222-3333-4444-5555-6666-7777-9999")
            .expect("a well-formed SID converts");
        assert!(!already_traversable(&root, sid.raw()));

        grant_traverse(&root, sid.raw()).expect("grant traverse");
        assert!(already_traversable(&root, sid.raw()));
        assert_eq!(rights_of(&root, sid.raw()) & FILE_GENERIC_WRITE, 0);

        // `NO_INHERITANCE`: what the directory holds gains nothing.
        let child = root.join("child");
        std::fs::create_dir_all(&child).expect("create child");
        assert_eq!(rights_of(&child, sid.raw()) & TRAVERSE_RIGHTS, 0);

        revoke(&root, sid.raw()).expect("revoke");
        assert_eq!(rights_of(&root, sid.raw()) & TRAVERSE_RIGHTS, 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Lowering the label and putting it back is a round trip: an
    /// object that carried no label carries none again.
    #[test]
    fn an_integrity_label_is_restored_to_what_it_was() {
        let root = std::env::temp_dir().join("gos-sandbox-acl-label");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create root");
        let before = integrity_label(&root);

        set_low_integrity(&root).expect("lower the label");
        let lowered = integrity_label(&root);
        assert!(lowered.contains(";LW)"), "{lowered:?}");

        restore_integrity(&root, &before).expect("restore the label");
        assert_eq!(integrity_label(&root), before);
        let _ = std::fs::remove_dir_all(&root);
    }
}
