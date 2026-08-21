#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]

//! `std::sandbox` C-ABI shims over `gossamer-sandbox`.
//!
//! A `sandbox::Policy` crosses the ABI as an `i64` handle into the
//! registry below, exactly as `fs::File` and `process::Child` do. Each
//! builder call consumes its handle and answers a new one, so the
//! Gossamer-side value is always the policy as it stands and a
//! discarded intermediate is reclaimed rather than leaked.

use std::os::raw::c_char;
use std::sync::Mutex;

use gossamer_sandbox::{Level, Network, SandboxPolicy, Stdio};

use super::string::alloc_cstring;
use super::vec::{GosVec, gos_rt_result_new};

/// Live policies, keyed by the handle Gossamer holds.
fn registry() -> &'static Mutex<(i64, std::collections::HashMap<i64, SandboxPolicy>)> {
    static REGISTRY: std::sync::OnceLock<
        Mutex<(i64, std::collections::HashMap<i64, SandboxPolicy>)>,
    > = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new((1, std::collections::HashMap::new())))
}

fn insert(policy: SandboxPolicy) -> i64 {
    let mut guard = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let handle = guard.0;
    guard.0 += 1;
    guard.1.insert(handle, policy);
    handle
}

fn take(handle: i64) -> Option<SandboxPolicy> {
    registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .1
        .remove(&handle)
}

fn peek(handle: i64) -> Option<SandboxPolicy> {
    registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .1
        .get(&handle)
        .cloned()
}

/// Applies `edit` to the policy `handle` names and answers a fresh
/// handle.
///
/// The old handle is consumed: a builder chain produces one live
/// policy, and keeping every intermediate would grow the registry for
/// the length of the program.
fn edit(handle: i64, edit: impl FnOnce(SandboxPolicy) -> SandboxPolicy) -> i64 {
    match take(handle) {
        Some(policy) => insert(edit(policy)),
        None => 0,
    }
}

unsafe fn text(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { crate::c_abi::gos_str_arg_string(ptr) }
    }
}

/// `sandbox::Policy::new()` - nothing reachable, no network, a private
/// temp, level `standard`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_new() -> i64 {
    insert(SandboxPolicy::new())
}

/// `sandbox::Policy::build_default(root)` - the policy
/// `gos build --sandbox` compiles under.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_policy_build_default(root: *const c_char) -> i64 {
    let root = std::path::PathBuf::from(unsafe { text(root) });
    let caches = default_cache_roots();
    let toolchain = default_toolchain_roots();
    insert(SandboxPolicy::build_default(&root, &caches, &toolchain))
}

/// `sandbox::Policy::command_default(cwd)` - the working directory
/// read-write, the network denied, HOME and the credentials denied.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_policy_command_default(cwd: *const c_char) -> i64 {
    let cwd = std::path::PathBuf::from(unsafe { text(cwd) });
    insert(SandboxPolicy::command_default(&cwd))
}

fn default_cache_roots() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = gossamer_sandbox::home_directory() {
        roots.push(home.join(".gossamer").join("cache"));
        roots.push(home.join(".cargo").join("registry"));
        roots.push(home.join(".cargo").join("git"));
    }
    roots
}

fn default_toolchain_roots() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    if let Some(sysroot) = gossamer_sandbox::discover::query("rustc", &["--print", "sysroot"]) {
        roots.push(std::path::PathBuf::from(sysroot));
    }
    roots
}

/// `policy.read_write(path)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_policy_read_write(handle: i64, path: *const c_char) -> i64 {
    let path = unsafe { text(path) };
    edit(handle, |policy| policy.read_write(path))
}

/// `policy.read_only(path)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_policy_read_only(handle: i64, path: *const c_char) -> i64 {
    let path = unsafe { text(path) };
    edit(handle, |policy| policy.read_only(path))
}

/// `policy.deny(path)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_policy_deny(handle: i64, path: *const c_char) -> i64 {
    let path = unsafe { text(path) };
    edit(handle, |policy| policy.deny(path))
}

/// `policy.network(allow)`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_network(handle: i64, allow: i64) -> i64 {
    let network = if allow == 0 {
        Network::None
    } else {
        Network::Open
    };
    edit(handle, |policy| policy.network(network))
}

/// `policy.env_allow(name)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_policy_env_allow(handle: i64, name: *const c_char) -> i64 {
    let name = unsafe { text(name) };
    edit(handle, |policy| policy.env_allow([name]))
}

/// `policy.env_set(name, value)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_policy_env_set(
    handle: i64,
    name: *const c_char,
    value: *const c_char,
) -> i64 {
    let name = unsafe { text(name) };
    let value = unsafe { text(value) };
    edit(handle, |policy| policy.env_set(name, value))
}

/// `policy.timeout(milliseconds)`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_timeout(handle: i64, milliseconds: i64) -> i64 {
    let limit = std::time::Duration::from_millis(milliseconds.max(0) as u64);
    edit(handle, |policy| policy.timeout(limit))
}

/// `policy.level(name)`. An unknown name leaves the level unchanged
/// rather than silently weakening it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_policy_level(handle: i64, name: *const c_char) -> i64 {
    let requested = Level::parse(&unsafe { text(name) });
    edit(handle, |policy| match requested {
        Some(level) => policy.level(level),
        None => policy,
    })
}

/// `policy.working_directory(path)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_policy_working_directory(
    handle: i64,
    path: *const c_char,
) -> i64 {
    let path = unsafe { text(path) };
    edit(handle, |policy| policy.working_directory(path))
}

/// `policy.explain()` - the compiled policy and the mechanisms a run
/// would install, or the reason it cannot be compiled.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_explain(handle: i64) -> *mut c_char {
    let Some(policy) = peek(handle) else {
        return alloc_cstring(b"sandbox: no such policy");
    };
    let text = match gossamer_sandbox::Sandbox::new(&policy) {
        Ok(sandbox) => {
            let compiled = sandbox.policy();
            let mut out = format!("level {}\n", compiled.level);
            for line in sandbox.mechanisms() {
                out.push_str(&format!("mechanism {line}\n"));
            }
            for rule in compiled.grants() {
                out.push_str(&format!(
                    "{} {}\n",
                    match rule.access {
                        gossamer_sandbox::Access::ReadWrite => "read-write",
                        _ => "read-only",
                    },
                    rule.path.display()
                ));
            }
            for rule in compiled.denials() {
                out.push_str(&format!("denied {}\n", rule.path.display()));
            }
            out
        }
        Err(error) => error.to_string(),
    };
    alloc_cstring(text.as_bytes())
}

/// `sandbox::run(policy, argv) -> Result<Output, errors::Error>`.
///
/// The Ok payload is the same three-slot `[stdout, stderr, code]`
/// aggregate `process::run` answers, so a caller reads `out.stdout`
/// and `out.code` with the field projection that already exists.
///
/// The call blocks for the length of the child, so it runs on the
/// blocking pool rather than on a scheduler worker: without that, one
/// sandboxed build would hold a worker for the whole build.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_run(handle: i64, argv: *mut GosVec) -> i128 {
    let fail = |message: String| {
        let err = crate::c_abi::errors::error_new_from_bytes(message.as_bytes());
        unsafe { gos_rt_result_new(1, err as i64) }
    };
    let Some(policy) = peek(handle) else {
        return fail("sandbox::run: no such policy".to_string());
    };
    let argv = unsafe { read_string_vec(argv) };
    if argv.is_empty() {
        return fail("sandbox::run: no command to run".to_string());
    }
    let sandbox = match gossamer_sandbox::Sandbox::new(&policy) {
        Ok(sandbox) => sandbox,
        Err(error) => return fail(error.to_string()),
    };
    match crate::sched_global::run_blocking("sandbox-run", move || {
        sandbox.run_with(&argv, Stdio::Capture)
    }) {
        Ok(Ok(output)) => {
            let stdout = alloc_cstring(output.stdout_text().as_bytes()) as i64;
            let stderr = alloc_cstring(output.stderr_text().as_bytes()) as i64;
            let blob =
                Box::into_raw(Box::new([stdout, stderr, i64::from(output.code)])).cast::<i64>();
            unsafe { gos_rt_result_new(0, blob as i64) }
        }
        Ok(Err(error)) => fail(error.to_string()),
        Err(error) => fail(format!("sandbox::run: {error}")),
    }
}

unsafe fn read_string_vec(v: *mut GosVec) -> Vec<String> {
    if v.is_null() {
        return Vec::new();
    }
    let vref = unsafe { &*v };
    if vref.ptr.is_null() || vref.len <= 0 {
        return Vec::new();
    }
    let elem_bytes = vref.elem_bytes as usize;
    if elem_bytes == 0 {
        return Vec::new();
    }
    (0..vref.len)
        .map(|index| {
            let slot = unsafe { vref.ptr.add((index as usize) * elem_bytes) };
            let ptr = unsafe {
                std::ptr::with_exposed_provenance::<c_char>((slot as *const usize).read_unaligned())
            };
            unsafe { text(ptr) }
        })
        .collect()
}

/// `sandbox::max_level()` - the highest level this host can honor.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_max_level() -> *mut c_char {
    alloc_cstring(
        gossamer_sandbox::capabilities()
            .max_level
            .as_str()
            .as_bytes(),
    )
}

/// `sandbox::platform()` - which backend answers here.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_platform() -> *mut c_char {
    alloc_cstring(
        gossamer_sandbox::capabilities()
            .platform
            .to_string()
            .as_bytes(),
    )
}

/// `sandbox::filesystem()` - how completely a filesystem policy is
/// enforced here.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_filesystem() -> *mut c_char {
    alloc_cstring(
        gossamer_sandbox::capabilities()
            .filesystem
            .to_string()
            .as_bytes(),
    )
}

/// `sandbox::network_enforcement()` - how completely network denial is
/// enforced here.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_network_enforcement() -> *mut c_char {
    alloc_cstring(
        gossamer_sandbox::capabilities()
            .network
            .to_string()
            .as_bytes(),
    )
}

/// `sandbox::process_isolation()` - how completely the process table
/// is isolated here.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_process_isolation() -> *mut c_char {
    alloc_cstring(
        gossamer_sandbox::capabilities()
            .process_isolation
            .to_string()
            .as_bytes(),
    )
}

/// `sandbox::resource_limits()` - how completely resource limits are
/// enforced here.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_resource_limits() -> *mut c_char {
    alloc_cstring(
        gossamer_sandbox::capabilities()
            .resource_limits
            .to_string()
            .as_bytes(),
    )
}

/// `sandbox::capabilities_json()` - the whole report, for a program
/// that wants more than the scalar accessors give it.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_capabilities_json() -> *mut c_char {
    alloc_cstring(gossamer_sandbox::capabilities().to_json().as_bytes())
}

/// `sandbox::notes()` - everything the scalar accessors cannot say,
/// such as which Landlock ABI the kernel reports or which sysctl
/// blocks `strict`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_notes() -> *mut GosVec {
    let notes = gossamer_sandbox::capabilities().notes;
    let vec = unsafe {
        crate::c_abi::vec::gos_rt_vec_with_capacity_typed(
            8,
            notes.len() as i64,
            crate::c_abi::vec::vec_elem_kind::STRING,
        )
    };
    for note in &notes {
        let element = alloc_cstring(note.as_bytes()) as i64;
        unsafe {
            crate::c_abi::vec::gos_rt_vec_push(vec, std::ptr::addr_of!(element).cast::<u8>());
        }
    }
    vec
}

#[cfg(test)]
mod sandbox_abi_tests {
    use super::*;

    #[test]
    fn a_builder_chain_consumes_each_handle_and_answers_a_live_one() {
        let first = gos_rt_sandbox_policy_new();
        assert!(first > 0);
        let second = gos_rt_sandbox_policy_network(first, 1);
        assert!(second > 0);
        assert_ne!(first, second);
        assert!(peek(first).is_none(), "the consumed handle is reclaimed");
        assert_eq!(
            peek(second).expect("the new handle is live").network,
            Network::Open
        );
    }

    #[test]
    fn an_unknown_level_name_never_weakens_the_policy() {
        let handle = gos_rt_sandbox_policy_new();
        let name = std::ffi::CString::new("paranoid").expect("cstring");
        let next = unsafe { gos_rt_sandbox_policy_level(handle, name.as_ptr()) };
        assert_eq!(
            peek(next).expect("live").level,
            gossamer_sandbox::Level::Standard
        );
    }

    #[test]
    fn a_handle_that_names_nothing_answers_zero_rather_than_panicking() {
        assert_eq!(gos_rt_sandbox_policy_network(999_999, 1), 0);
    }
}
