//! Creating a child with a restricted token or inside an
//! `AppContainer`.
//!
//! `std::process::Command` cannot express either: a token has to be
//! attached at creation, and an `AppContainer` is a `SECURITY_CAPABILITIES`
//! entry in an extended startup-info attribute list. Both are what the
//! `standard` and `strict` levels mean on Windows, so the backend
//! creates the process itself.
//!
//! Handle inheritance uses an explicit `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`
//! rather than bare `bInheritHandles`, which would inherit every
//! inheritable handle the process happens to hold.

#![allow(
    unsafe_code,
    reason = "process creation with a token is a raw Win32 API; every \
              call passes stack-owned structs with their sizes, which is \
              the documented contract"
)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::path::Path;

use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, FALSE, HANDLE, INVALID_HANDLE_VALUE, TRUE,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::{SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_GENERIC_WRITE, OPEN_EXISTING};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetExitCodeProcess, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

use crate::exec::{ChildProcess, Exit, Stdio};
use crate::policy::CompiledPolicy;

use super::token::Token;

/// `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`.
const ATTRIBUTE_HANDLE_LIST: usize = 0x0002_0002;
/// `PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY`.
const ATTRIBUTE_MITIGATION_POLICY: usize = 0x0002_0007;

/// Mitigation policies applied to every sandboxed child.
///
/// Each removes an execution path the child never needs and an
/// attacker does: a dynamic code page, or an extension-point DLL that
/// a third party injects into any process that starts.
///
/// `BLOCK_NON_MICROSOFT_BINARIES` is deliberately absent. It bars every
/// image and DLL Microsoft did not sign, which is what a sandbox exists
/// to run: a toolchain, a build script, a linker, a test binary. Its
/// protection is signature provenance, and the policy already decides
/// what the child may reach by path.
const MITIGATIONS: u64 = PROCESS_CREATION_MITIGATION_POLICY_PROHIBIT_DYNAMIC_CODE_ALWAYS_ON
    | PROCESS_CREATION_MITIGATION_POLICY_EXTENSION_POINT_DISABLE_ALWAYS_ON;

const PROCESS_CREATION_MITIGATION_POLICY_PROHIBIT_DYNAMIC_CODE_ALWAYS_ON: u64 = 0x01 << 36;
const PROCESS_CREATION_MITIGATION_POLICY_EXTENSION_POINT_DISABLE_ALWAYS_ON: u64 = 0x01 << 32;

/// A child created directly through Win32.
pub(crate) struct RawChild {
    process: HANDLE,
    thread: HANDLE,
    stdout: Option<std::fs::File>,
    stderr: Option<std::fs::File>,
    exited: Option<i32>,
}

// The handles are owned by this value and Win32 handles have no
// thread affinity.
#[allow(
    unsafe_code,
    reason = "a Win32 HANDLE is a process-wide token with no thread affinity"
)]
unsafe impl Send for RawChild {}

impl RawChild {
    /// The process handle, so the job object can claim the tree.
    pub(crate) const fn process(&self) -> HANDLE {
        self.process
    }
}

impl ChildProcess for RawChild {
    fn poll(&mut self) -> std::io::Result<Option<Exit>> {
        if let Some(code) = self.exited {
            return Ok(Some(Exit::Code(code)));
        }
        let waited = unsafe { WaitForSingleObject(self.process, 0) };
        if waited == WAIT_TIMEOUT {
            return Ok(None);
        }
        if waited != WAIT_OBJECT_0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut code = 0u32;
        if unsafe { GetExitCodeProcess(self.process, &raw mut code) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let code = code as i32;
        self.exited = Some(code);
        Ok(Some(Exit::Code(code)))
    }

    fn wait(&mut self) -> std::io::Result<Exit> {
        if let Some(code) = self.exited {
            return Ok(Exit::Code(code));
        }
        if unsafe { WaitForSingleObject(self.process, u32::MAX) } != WAIT_OBJECT_0 {
            return Err(std::io::Error::last_os_error());
        }
        self.poll()?
            .ok_or_else(|| std::io::Error::other("the child reported no exit code"))
    }

    fn kill_tree(&mut self) {
        // The job object the backend assigned owns the tree; this ends
        // the root so the job's kill-on-close has nothing left to wait
        // for.
        unsafe { TerminateProcess(self.process, 1) };
    }

    fn take_stdout(&mut self) -> Option<Box<dyn std::io::Read + Send>> {
        self.stdout
            .take()
            .map(|file| Box::new(file) as Box<dyn std::io::Read + Send>)
    }

    fn take_stderr(&mut self) -> Option<Box<dyn std::io::Read + Send>> {
        self.stderr
            .take()
            .map(|file| Box::new(file) as Box<dyn std::io::Read + Send>)
    }
}

impl Drop for RawChild {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.thread);
            CloseHandle(self.process);
        }
    }
}

/// Creates `argv` under `token`, with `capabilities` when the level is
/// `AppContainer`.
///
/// `capabilities` is the `SECURITY_CAPABILITIES` the container needs;
/// `None` means a restricted token without a container, which is what
/// `standard` is.
pub(crate) fn spawn(
    policy: &CompiledPolicy,
    argv: &[String],
    stdio: Stdio,
    token: &Token,
    capabilities: Option<*mut std::ffi::c_void>,
) -> Result<RawChild, String> {
    let command_line = wide(&quote_command_line(argv));
    let environment = environment_block(policy, capabilities.is_some());
    let working_directory = policy
        .working_directory
        .as_deref()
        .map(|path| wide(&path.to_string_lossy()));

    let (stdout_read, stdout_write) = stream_pair(stdio)?;
    let (stderr_read, stderr_write) = stream_pair(stdio)?;
    let stdin_handle = null_device()?;

    let mut inherited = [stdin_handle, stdout_write, stderr_write];
    let mut attributes = AttributeList::new(if capabilities.is_some() { 3 } else { 2 })?;
    attributes.set_handle_list(&mut inherited)?;
    attributes.set_mitigations(MITIGATIONS)?;
    if let Some(security) = capabilities {
        attributes.set_security_capabilities(security)?;
    }

    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = u32::try_from(std::mem::size_of::<STARTUPINFOEXW>()).unwrap_or(0);
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin_handle;
    startup.StartupInfo.hStdOutput = stdout_write;
    startup.StartupInfo.hStdError = stderr_write;
    startup.lpAttributeList = attributes.raw();

    let mut information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let created = unsafe {
        CreateProcessAsUserW(
            token.handle(),
            std::ptr::null(),
            command_line.as_ptr().cast_mut(),
            std::ptr::null::<SECURITY_ATTRIBUTES>(),
            std::ptr::null::<SECURITY_ATTRIBUTES>(),
            TRUE,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_NEW_PROCESS_GROUP,
            environment.as_ptr().cast::<std::ffi::c_void>().cast_mut(),
            working_directory
                .as_ref()
                .map_or(std::ptr::null(), std::vec::Vec::as_ptr),
            std::ptr::addr_of_mut!(startup).cast::<STARTUPINFOW>(),
            &raw mut information,
        )
    };
    // The child owns the write ends now; keeping them open here would
    // stop the reads from ever seeing end-of-file.
    unsafe {
        CloseHandle(stdout_write);
        CloseHandle(stderr_write);
        CloseHandle(stdin_handle);
    }
    if created == 0 {
        return Err(format!(
            "CreateProcessAsUser failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    Ok(RawChild {
        process: information.hProcess,
        thread: information.hThread,
        stdout: stdout_read.map(std::fs::File::from),
        stderr: stderr_read.map(std::fs::File::from),
        exited: None,
    })
}

/// The read and write ends of a captured stream, or a handle to the
/// null device when the stream is not captured.
fn stream_pair(stdio: Stdio) -> Result<(Option<OwnedHandle>, HANDLE), String> {
    match stdio {
        Stdio::Capture => {
            let mut read: HANDLE = std::ptr::null_mut();
            let mut write: HANDLE = std::ptr::null_mut();
            let mut security = SECURITY_ATTRIBUTES {
                nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
                lpSecurityDescriptor: std::ptr::null_mut(),
                bInheritHandle: TRUE,
            };
            if unsafe { CreatePipe(&raw mut read, &raw mut write, &raw mut security, 0) } == 0 {
                return Err(format!(
                    "CreatePipe failed: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // Both ends are inheritable, and only the write end is named
            // in the handle list, which is the whole point of the list:
            // the child gets what it is given and nothing else.
            let read = unsafe { OwnedHandle::from_raw_handle(read.cast()) };
            Ok((Some(read), write))
        }
        Stdio::Inherit => Ok((None, inherited_standard_handle())),
        Stdio::Null => Ok((None, null_device()?)),
    }
}

/// The caller's own standard output handle, duplicated so the child's
/// list owns an inheritable copy.
fn inherited_standard_handle() -> HANDLE {
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
    let source = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    let mut duplicate: HANDLE = std::ptr::null_mut();
    unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            source,
            GetCurrentProcess(),
            &raw mut duplicate,
            0,
            TRUE,
            DUPLICATE_SAME_ACCESS,
        );
    }
    duplicate
}

/// An inheritable handle to `NUL`.
fn null_device() -> Result<HANDLE, String> {
    let name = wide("NUL");
    let mut security = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: TRUE,
    };
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            FILE_GENERIC_WRITE,
            0,
            &raw mut security,
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(format!(
            "opening NUL failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(handle)
}

/// An owned `PROC_THREAD_ATTRIBUTE_LIST`.
struct AttributeList {
    buffer: Vec<u8>,
    initialized: bool,
}

impl AttributeList {
    fn new(count: u32) -> Result<Self, String> {
        let mut size = 0usize;
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &raw mut size);
        }
        let mut list = Self {
            buffer: vec![0u8; size],
            initialized: false,
        };
        let started =
            unsafe { InitializeProcThreadAttributeList(list.raw(), count, 0, &raw mut size) };
        if started == 0 {
            return Err(format!(
                "InitializeProcThreadAttributeList failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        list.initialized = true;
        Ok(list)
    }

    fn raw(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.buffer.as_mut_ptr().cast()
    }

    /// The exact set of handles the child inherits, rather than every
    /// inheritable handle this process holds.
    fn set_handle_list(&mut self, handles: &mut [HANDLE]) -> Result<(), String> {
        self.update(
            ATTRIBUTE_HANDLE_LIST,
            handles.as_mut_ptr().cast(),
            std::mem::size_of_val(handles),
        )
    }

    fn set_mitigations(&mut self, policy: u64) -> Result<(), String> {
        // The value must outlive the call, and `CreateProcess` reads it
        // again, so it is boxed and leaked into the list's lifetime.
        let boxed = Box::leak(Box::new(policy));
        self.update(
            ATTRIBUTE_MITIGATION_POLICY,
            std::ptr::from_mut(boxed).cast(),
            std::mem::size_of::<u64>(),
        )
    }

    fn set_security_capabilities(
        &mut self,
        capabilities: *mut std::ffi::c_void,
    ) -> Result<(), String> {
        /// `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`.
        const ATTRIBUTE_SECURITY_CAPABILITIES: usize = 0x0002_0009;
        // The size is the structure's own, which is three machine words
        // rather than four: the count and the reserved word share one.
        self.update(
            ATTRIBUTE_SECURITY_CAPABILITIES,
            capabilities,
            std::mem::size_of::<SECURITY_CAPABILITIES>(),
        )
    }

    fn update(
        &mut self,
        attribute: usize,
        value: *mut std::ffi::c_void,
        size: usize,
    ) -> Result<(), String> {
        let updated = unsafe {
            UpdateProcThreadAttribute(
                self.raw(),
                0,
                attribute,
                value,
                size,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if updated == 0 {
            return Err(format!(
                "UpdateProcThreadAttribute failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        if self.initialized {
            unsafe { DeleteProcThreadAttributeList(self.raw()) };
        }
    }
}

/// Variables Windows rewrites while it builds an `AppContainer` child.
///
/// Each is pointed at a directory inside the container profile, so the
/// value the child reads is the container's, never the host's. The
/// rewrite is a lookup in the block the caller supplies: a block that
/// names none of them leaves creation with nothing to rewrite, which is
/// `ERROR_ENVVAR_NOT_FOUND`.
const APP_CONTAINER_REDIRECTED_ENVIRONMENT: &[&str] = &["LOCALAPPDATA", "TEMP", "TMP"];

/// The child's environment as a NUL-separated, double-NUL-terminated
/// UTF-16 block, which is what `CREATE_UNICODE_ENVIRONMENT` reads.
///
/// An `AppContainer` child also carries whatever the container profile
/// redirects, whether or not the policy allows those names through.
fn environment_block(policy: &CompiledPolicy, app_container: bool) -> Vec<u16> {
    let mut variables = policy.environment();
    if app_container {
        for name in APP_CONTAINER_REDIRECTED_ENVIRONMENT {
            if let Ok(value) = std::env::var(name) {
                variables.entry((*name).to_string()).or_insert(value);
            }
        }
    }
    encode_environment(&variables)
}

/// `NAME=VALUE` pairs as the block `CREATE_UNICODE_ENVIRONMENT` reads.
fn encode_environment(variables: &std::collections::BTreeMap<String, String>) -> Vec<u16> {
    let mut block: Vec<u16> = Vec::new();
    for (name, value) in variables {
        block.extend(wide(&format!("{name}={value}")));
    }
    // The block ends with an empty string, so an environment with no
    // variables is that terminator alone: two NULs, not one.
    if variables.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

/// Renders `argv` as a Windows command line.
///
/// Windows hands the child one string and lets it parse; a path with a
/// space that is not quoted becomes two arguments, so quoting is part
/// of the contract rather than cosmetic.
fn quote_command_line(argv: &[String]) -> String {
    argv.iter()
        .map(|argument| {
            if argument.is_empty() || argument.contains(|ch: char| ch.is_whitespace() || ch == '"')
            {
                let escaped = argument.replace('\\', "\\\\").replace('"', "\\\"");
                format!("\"{escaped}\"")
            } else {
                argument.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A NUL-terminated UTF-16 copy of `text`.
fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Silences the unused-import warning for `Path` and `FALSE`, both of
/// which name parts of the contract above.
const _: (Option<&Path>, i32) = (None, FALSE);

#[cfg(test)]
mod spawn_tests {
    use super::*;

    #[test]
    fn an_argument_with_a_space_is_quoted() {
        assert_eq!(
            quote_command_line(&["C:\\Program Files\\x.exe".to_string(), "a b".to_string()]),
            "\"C:\\\\Program Files\\\\x.exe\" \"a b\""
        );
    }

    #[test]
    fn an_environment_block_is_double_nul_terminated() {
        let variables = std::collections::BTreeMap::from([("A".to_string(), "1".to_string())]);
        let mut expected = wide("A=1");
        expected.push(0);
        assert_eq!(encode_environment(&variables), expected);
    }

    #[test]
    fn an_environment_with_no_variables_is_still_two_nuls() {
        assert_eq!(
            encode_environment(&std::collections::BTreeMap::new()),
            vec![0, 0]
        );
    }

    #[test]
    fn an_app_container_block_carries_what_the_profile_redirects() {
        let policy = crate::policy::SandboxPolicy::new()
            .compile()
            .expect("compile");
        let block = String::from_utf16_lossy(&environment_block(&policy, true));
        for name in APP_CONTAINER_REDIRECTED_ENVIRONMENT {
            if std::env::var(name).is_ok() {
                assert!(block.contains(&format!("{name}=")), "{name} in {block:?}");
            }
        }
    }
}
