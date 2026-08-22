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

use gossamer_sandbox::{Access, Level, Network, SandboxPolicy, Stdio, Temp};

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
    gossamer_sandbox::discover::rust_toolchain_paths()
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

// ---------------------------------------------------------------------
// Policy builders the first cut left out.
//
// Each one edits a field the library already exposes, so the binding
// widens what a Gossamer caller can say without changing what any
// backend enforces.
// ---------------------------------------------------------------------

/// `policy.network_mode(name)` - `none`, `client`, or `open`.
///
/// The three-way form of `network(allow)`: `client` is outbound-only,
/// which is what a dependency fetch needs and what a service does not.
/// An unknown name leaves the setting unchanged, so a typo can never
/// open the network a policy meant to close.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_policy_network_mode(
    handle: i64,
    name: *const c_char,
) -> i64 {
    let requested = parse_network(&unsafe { text(name) });
    edit(handle, |policy| match requested {
        Some(network) => policy.network(network),
        None => policy,
    })
}

/// The `Network` mode `name` spells, or `None` when it spells nothing.
fn parse_network(name: &str) -> Option<Network> {
    match name {
        "none" => Some(Network::None),
        "client" => Some(Network::Client),
        "open" => Some(Network::Open),
        _ => None,
    }
}

/// `policy.for_fetch_phase()` - the outbound-only network and the
/// resolver files a name lookup needs.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_for_fetch_phase(handle: i64) -> i64 {
    edit(handle, SandboxPolicy::for_fetch_phase)
}

/// `policy.temp(mode)` - `private` or `inherit`.
///
/// An unknown name leaves the choice unchanged, so a typo never turns a
/// private temp into the caller's own.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_policy_temp(handle: i64, mode: *const c_char) -> i64 {
    let requested = match unsafe { text(mode) }.as_str() {
        "private" => Some(Temp::Private),
        "inherit" => Some(Temp::Inherit),
        _ => None,
    };
    edit(handle, |policy| match requested {
        Some(temp) => policy.temp(temp),
        None => policy,
    })
}

/// `policy.temp_path(path)` - a caller-chosen temporary directory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_policy_temp_path(handle: i64, path: *const c_char) -> i64 {
    let path = std::path::PathBuf::from(unsafe { text(path) });
    edit(handle, |policy| policy.temp(Temp::Path(path)))
}

/// `policy.max_processes(count)`. A count at or below zero clears the
/// limit rather than asking for one nothing can satisfy.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_max_processes(handle: i64, count: i64) -> i64 {
    edit(handle, move |mut policy| {
        // Saturating rather than truncating: a count past `u32` would
        // wrap to a tiny one, turning the largest bound a caller can
        // write into the most restrictive one.
        policy.resources.max_processes =
            positive(count).map(|value| u32::try_from(value).unwrap_or(u32::MAX));
        policy
    })
}

/// `policy.max_memory(bytes)`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_max_memory(handle: i64, bytes: i64) -> i64 {
    edit(handle, move |mut policy| {
        policy.resources.max_memory = positive(bytes);
        policy
    })
}

/// `policy.max_file_size(bytes)`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_max_file_size(handle: i64, bytes: i64) -> i64 {
    edit(handle, move |mut policy| {
        policy.resources.max_file_size = positive(bytes);
        policy
    })
}

/// `policy.max_temp_size(bytes)`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_max_temp_size(handle: i64, bytes: i64) -> i64 {
    edit(handle, move |mut policy| {
        policy.resources.max_temp_size = positive(bytes);
        policy
    })
}

/// `policy.max_cpu_time(ms)`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_max_cpu_time(handle: i64, milliseconds: i64) -> i64 {
    edit(handle, move |mut policy| {
        policy.resources.max_cpu_time =
            positive(milliseconds).map(std::time::Duration::from_millis);
        policy
    })
}

/// The limit `value` asks for, or `None` when it asks for none.
///
/// Zero and below clear the limit: a caller that computed a bound and
/// got nothing means "unbounded", and a zero-byte memory cap would kill
/// every child before it started.
const fn positive(value: i64) -> Option<u64> {
    if value > 0 { Some(value as u64) } else { None }
}

// ---------------------------------------------------------------------
// Introspection: what the policy says, and what the host will honor.
// ---------------------------------------------------------------------

/// Compiles `handle`'s policy, or answers the reason it will not
/// compile.
fn compiled(handle: i64) -> Result<gossamer_sandbox::CompiledPolicy, String> {
    let policy = peek(handle).ok_or_else(|| "sandbox: no such policy".to_string())?;
    policy.compile().map_err(|error| error.to_string())
}

/// `policy.check() -> Result<(), errors::Error>`.
///
/// Answers what a run would refuse before anything is spawned: a path
/// that does not resolve, an environment variable no policy may pass,
/// or a level this host cannot honor. The level check is the reason
/// this is not just `compile`: a policy can be well-formed and still be
/// unrunnable here.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_check(handle: i64) -> i128 {
    let Some(policy) = peek(handle) else {
        return sandbox_err("sandbox: no such policy");
    };
    match gossamer_sandbox::Sandbox::new(&policy) {
        Ok(_) => unsafe { gos_rt_result_new(0, 0) },
        Err(error) => sandbox_err(&error.to_string()),
    }
}

/// An `Err(errors::Error)` carrying `message`.
fn sandbox_err(message: &str) -> i128 {
    let err = crate::c_abi::errors::error_new_from_bytes(message.as_bytes());
    unsafe { gos_rt_result_new(1, err as i64) }
}

/// `policy.mechanisms() -> Vec<String>` - what a run would install, in
/// the order it is applied.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_mechanisms(handle: i64) -> *mut GosVec {
    let lines = peek(handle)
        .and_then(|policy| gossamer_sandbox::Sandbox::new(&policy).ok())
        .map_or_else(Vec::new, |sandbox| sandbox.mechanisms());
    string_vec(&lines)
}

/// `policy.to_json()` - the compiled policy, for a report or a test.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_to_json(handle: i64) -> *mut c_char {
    match compiled(handle) {
        Ok(policy) => alloc_cstring(policy.to_json().as_bytes()),
        Err(reason) => alloc_cstring(reason.as_bytes()),
    }
}

/// `policy.access(path)` - `read-write`, `read-only`, or `deny`, the
/// verdict the compiled policy gives that exact path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_policy_access(
    handle: i64,
    path: *const c_char,
) -> *mut c_char {
    let path = std::path::PathBuf::from(unsafe { text(path) });
    let verdict = compiled(handle).map_or("deny", |policy| access_name(policy.access(&path)));
    alloc_cstring(verdict.as_bytes())
}

/// The spelling of `access` a Gossamer caller matches on.
const fn access_name(access: Access) -> &'static str {
    match access {
        Access::ReadOnly => "read-only",
        Access::ReadWrite => "read-write",
        Access::Deny => "deny",
    }
}

/// `policy.read_write_grants() -> Vec<String>`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_read_write_grants(handle: i64) -> *mut GosVec {
    string_vec(&rules(handle, Access::ReadWrite))
}

/// `policy.read_only_grants() -> Vec<String>`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_read_only_grants(handle: i64) -> *mut GosVec {
    string_vec(&rules(handle, Access::ReadOnly))
}

/// `policy.denials() -> Vec<String>`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_denials(handle: i64) -> *mut GosVec {
    string_vec(&rules(handle, Access::Deny))
}

/// The compiled paths carrying exactly `access`.
///
/// One list per access rather than one list of pairs: the access is
/// then the name of the call, and a caller never has to agree with the
/// binding about how a pair was spelled.
fn rules(handle: i64, access: Access) -> Vec<String> {
    let Ok(policy) = compiled(handle) else {
        return Vec::new();
    };
    let rules: Box<dyn Iterator<Item = &gossamer_sandbox::PathRule>> = if access == Access::Deny {
        Box::new(policy.denials())
    } else {
        Box::new(policy.grants())
    };
    rules
        .filter(|rule| rule.access == access)
        .map(|rule| rule.path.display().to_string())
        .collect()
}

/// `policy.environment_names() -> Vec<String>` - every name the child
/// will actually see, sorted.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_environment_names(handle: i64) -> *mut GosVec {
    let names = compiled(handle).map_or_else(
        |_| Vec::new(),
        |policy| policy.environment().into_keys().collect::<Vec<String>>(),
    );
    string_vec(&names)
}

/// `policy.environment_value(name)` - what the child will see for
/// `name`, or the empty string when it will see nothing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_policy_environment_value(
    handle: i64,
    name: *const c_char,
) -> *mut c_char {
    let name = unsafe { text(name) };
    let value = compiled(handle).map_or_else(
        |_| String::new(),
        |policy| policy.environment().remove(&name).unwrap_or_default(),
    );
    alloc_cstring(value.as_bytes())
}

/// `policy.level_name()` - the level the policy asks for.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_level_name(handle: i64) -> *mut c_char {
    let level = peek(handle).map_or(Level::None, |policy| policy.level);
    alloc_cstring(level.as_str().as_bytes())
}

/// `policy.network_name()` - `none`, `client`, or `open`, as asked for.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_network_name(handle: i64) -> *mut c_char {
    let network = peek(handle).map_or(Network::None, |policy| policy.network);
    alloc_cstring(network_name(network).as_bytes())
}

/// The spelling of `network` a Gossamer caller matches on.
const fn network_name(network: Network) -> &'static str {
    match network {
        Network::None => "none",
        Network::Client => "client",
        Network::Open => "open",
    }
}

/// `policy.working_directory_path()` - where the child starts, or the
/// empty string when the policy leaves it to the caller.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_working_directory_path(handle: i64) -> *mut c_char {
    let path = peek(handle)
        .and_then(|policy| policy.working_directory)
        .map_or_else(String::new, |path| path.display().to_string());
    alloc_cstring(path.as_bytes())
}

/// `policy.level_blocker()` - the primitive that stops this host from
/// honoring the level, or the empty string when nothing does.
///
/// The other half of the level contract: `sandbox::max_level()` says
/// what the host can do, this says what stands in the way.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_level_blocker(handle: i64) -> *mut c_char {
    let Some(policy) = peek(handle) else {
        return alloc_cstring(b"sandbox: no such policy");
    };
    let reason = match gossamer_sandbox::Sandbox::new(&policy) {
        Err(gossamer_sandbox::SandboxError::LevelUnavailable { reason, .. }) => reason,
        _ => String::new(),
    };
    alloc_cstring(reason.as_bytes())
}

/// `policy.network_enforcement_kind()` - how completely THIS run's
/// network setting is enforced, which is not what the policy asked for.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_network_enforcement_kind(handle: i64) -> *mut c_char {
    alloc_cstring(enforcement_kind(&policy_network_enforcement(handle)).as_bytes())
}

/// `policy.network_enforcement_reason()`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_network_enforcement_reason(handle: i64) -> *mut c_char {
    alloc_cstring(enforcement_reason(&policy_network_enforcement(handle)).as_bytes())
}

fn policy_network_enforcement(handle: i64) -> gossamer_sandbox::Enforcement {
    peek(handle)
        .and_then(|policy| gossamer_sandbox::Sandbox::new(&policy).ok())
        .map_or(gossamer_sandbox::Enforcement::None, |sandbox| {
            sandbox.network_enforcement()
        })
}

/// `policy.resource_enforcement_kind()` - whether every limit the
/// policy names will actually be applied here.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_resource_enforcement_kind(handle: i64) -> *mut c_char {
    alloc_cstring(enforcement_kind(&policy_resource_enforcement(handle)).as_bytes())
}

/// `policy.resource_enforcement_reason()`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_policy_resource_enforcement_reason(handle: i64) -> *mut c_char {
    alloc_cstring(enforcement_reason(&policy_resource_enforcement(handle)).as_bytes())
}

fn policy_resource_enforcement(handle: i64) -> gossamer_sandbox::Enforcement {
    peek(handle).map_or(gossamer_sandbox::Enforcement::None, |policy| {
        gossamer_sandbox::resource_enforcement(&policy.resources, policy.level)
    })
}

/// `full`, `partial`, or `none` - the arm of an [`Enforcement`], for a
/// caller that matches on the verdict rather than printing it.
///
/// [`Enforcement`]: gossamer_sandbox::Enforcement
pub(crate) fn enforcement_kind(enforcement: &gossamer_sandbox::Enforcement) -> &'static str {
    match enforcement {
        gossamer_sandbox::Enforcement::Full => "full",
        gossamer_sandbox::Enforcement::Partial(_) => "partial",
        gossamer_sandbox::Enforcement::None => "none",
    }
}

/// What a `partial` verdict does not cover, or the empty string when
/// the verdict carries no reason.
pub(crate) fn enforcement_reason(enforcement: &gossamer_sandbox::Enforcement) -> String {
    match enforcement {
        gossamer_sandbox::Enforcement::Partial(reason) => reason.clone(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------
// The host report, beyond the scalars the first cut exposed.
// ---------------------------------------------------------------------

/// `sandbox::os_description()` - the host as the report names it.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_os_description() -> *mut c_char {
    alloc_cstring(gossamer_sandbox::capabilities().os_description.as_bytes())
}

/// `sandbox::filesystem_kind()`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_filesystem_kind() -> *mut c_char {
    alloc_cstring(enforcement_kind(&gossamer_sandbox::capabilities().filesystem).as_bytes())
}

/// `sandbox::filesystem_reason()`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_filesystem_reason() -> *mut c_char {
    alloc_cstring(enforcement_reason(&gossamer_sandbox::capabilities().filesystem).as_bytes())
}

/// `sandbox::network_kind()`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_network_kind() -> *mut c_char {
    alloc_cstring(enforcement_kind(&gossamer_sandbox::capabilities().network).as_bytes())
}

/// `sandbox::network_reason()`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_network_reason() -> *mut c_char {
    alloc_cstring(enforcement_reason(&gossamer_sandbox::capabilities().network).as_bytes())
}

/// `sandbox::process_isolation_kind()`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_process_isolation_kind() -> *mut c_char {
    alloc_cstring(enforcement_kind(&gossamer_sandbox::capabilities().process_isolation).as_bytes())
}

/// `sandbox::process_isolation_reason()`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_process_isolation_reason() -> *mut c_char {
    alloc_cstring(
        enforcement_reason(&gossamer_sandbox::capabilities().process_isolation).as_bytes(),
    )
}

/// `sandbox::resource_limits_kind()`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_resource_limits_kind() -> *mut c_char {
    alloc_cstring(enforcement_kind(&gossamer_sandbox::capabilities().resource_limits).as_bytes())
}

/// `sandbox::resource_limits_reason()`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_resource_limits_reason() -> *mut c_char {
    alloc_cstring(enforcement_reason(&gossamer_sandbox::capabilities().resource_limits).as_bytes())
}

// ---------------------------------------------------------------------
// Discovery, so a profile can name a path the way an operator writes it.
// ---------------------------------------------------------------------

/// `sandbox::expand(text) -> Option<String>` - a written path with
/// `~` and the environment resolved.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_expand(text_ptr: *const c_char) -> i128 {
    optional_path(gossamer_sandbox::discover::expand(&unsafe {
        text(text_ptr)
    }))
}

/// `sandbox::prefix_of(name) -> Option<String>` - the install prefix of
/// a tool on `PATH`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_prefix_of(name: *const c_char) -> i128 {
    optional_path(gossamer_sandbox::discover::prefix_of(&unsafe {
        text(name)
    }))
}

/// `sandbox::resolve_on_path(name) -> Option<String>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_resolve_on_path(name: *const c_char) -> i128 {
    optional_path(gossamer_sandbox::discover::resolve_on_path(&unsafe {
        text(name)
    }))
}

/// `sandbox::home_directory() -> Option<String>`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_home_directory() -> i128 {
    optional_path(gossamer_sandbox::home_directory())
}

/// `sandbox::rust_toolchain_paths() -> Vec<String>`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_rust_toolchain_paths() -> *mut GosVec {
    let paths: Vec<String> = default_toolchain_roots()
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    string_vec(&paths)
}

/// A `Option<String>` carrier over a path that may not exist.
fn optional_path(path: Option<std::path::PathBuf>) -> i128 {
    match path {
        Some(path) => {
            let text = alloc_cstring(path.display().to_string().as_bytes());
            unsafe { gos_rt_result_new(0, text as i64) }
        }
        None => unsafe { gos_rt_result_new(1, 0) },
    }
}

/// A `Vec<String>` over `values`.
fn string_vec(values: &[String]) -> *mut GosVec {
    let vec = unsafe {
        crate::c_abi::vec::gos_rt_vec_with_capacity_typed(
            8,
            values.len() as i64,
            crate::c_abi::vec::vec_elem_kind::STRING,
        )
    };
    for value in values {
        let element = alloc_cstring(value.as_bytes()) as i64;
        unsafe {
            crate::c_abi::vec::gos_rt_vec_push(vec, std::ptr::addr_of!(element).cast::<u8>());
        }
    }
    vec
}

// ---------------------------------------------------------------------
// Host maintenance and the exit-code contract.
// ---------------------------------------------------------------------

/// `sandbox::stale_grant_count()` - how many interrupted runs left an
/// ACL grant behind. Always zero where grants are not how the backend
/// reaches a path.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_stale_grant_count() -> i64 {
    stale_grants() as i64
}

/// `sandbox::clean_stale_grants() -> Result<i64, errors::Error>` -
/// revokes them, answering how many were revoked.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_clean_stale_grants() -> i128 {
    match clean_grants() {
        Ok(count) => unsafe { gos_rt_result_new(0, count as i64) },
        Err(reason) => sandbox_err(&reason),
    }
}

#[cfg(windows)]
fn stale_grants() -> usize {
    gossamer_sandbox::windows::stale_grant_count()
}

/// Windows grants a path to the container by writing its ACL, so an
/// interrupted run can leave one behind. No other backend edits a
/// path's permissions, so no other backend has anything to clean.
#[cfg(not(windows))]
const fn stale_grants() -> usize {
    0
}

#[cfg(windows)]
fn clean_grants() -> Result<usize, String> {
    gossamer_sandbox::windows::clean_stale_grants()
}

#[cfg(not(windows))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the Windows twin edits ACLs and genuinely fails; both arms of a \
              cfg pair have to answer the same type"
)]
const fn clean_grants() -> Result<usize, String> {
    Ok(0)
}

/// `sandbox::exit_policy_error()`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_exit_policy_error() -> i64 {
    i64::from(gossamer_sandbox::EXIT_POLICY_ERROR)
}

/// `sandbox::exit_command_not_found()`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_exit_command_not_found() -> i64 {
    i64::from(gossamer_sandbox::EXIT_COMMAND_NOT_FOUND)
}

/// `sandbox::exit_level_unavailable()`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_exit_level_unavailable() -> i64 {
    i64::from(gossamer_sandbox::EXIT_LEVEL_UNAVAILABLE)
}

/// `sandbox::exit_signal_base()`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_sandbox_exit_signal_base() -> i64 {
    i64::from(gossamer_sandbox::EXIT_SIGNAL_BASE)
}

/// `sandbox::run_inherit(policy, argv) -> i64` - runs `argv` with the
/// caller's own streams and answers the exit code the contract gives.
///
/// The wrapper shape: the child writes straight to the terminal, and
/// every failure between the policy and the finished child maps to its
/// own code, so a policy mistake is never mistaken for a program that
/// merely failed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sandbox_run_inherit(handle: i64, argv: *mut GosVec) -> i64 {
    let Some(policy) = peek(handle) else {
        return i64::from(gossamer_sandbox::EXIT_POLICY_ERROR);
    };
    let argv = unsafe { read_string_vec(argv) };
    if argv.is_empty() {
        return i64::from(gossamer_sandbox::EXIT_POLICY_ERROR);
    }
    let sandbox = match gossamer_sandbox::Sandbox::new(&policy) {
        Ok(sandbox) => sandbox,
        Err(error) => return i64::from(error.exit_code()),
    };
    // The child writes to the same terminal this program buffers into,
    // so whatever the wrapper has already said has to be on the wire
    // before the child says anything.
    crate::c_abi::print::flush_stdout_buffer();
    let outcome = crate::sched_global::run_blocking("sandbox-run", move || {
        sandbox.run_with(&argv, Stdio::Inherit)
    });
    match outcome {
        Ok(outcome) => i64::from(gossamer_sandbox::exit_code_for(&outcome)),
        Err(_) => i64::from(gossamer_sandbox::EXIT_POLICY_ERROR),
    }
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

    #[test]
    fn an_unknown_network_mode_never_opens_a_closed_network() {
        let handle = gos_rt_sandbox_policy_new();
        let open = std::ffi::CString::new("open").expect("cstring");
        let typo = std::ffi::CString::new("opne").expect("cstring");
        let opened = unsafe { gos_rt_sandbox_policy_network_mode(handle, open.as_ptr()) };
        let after = unsafe { gos_rt_sandbox_policy_network_mode(opened, typo.as_ptr()) };
        assert_eq!(peek(after).expect("live").network, Network::Open);

        let handle = gos_rt_sandbox_policy_new();
        let next = unsafe { gos_rt_sandbox_policy_network_mode(handle, typo.as_ptr()) };
        assert_eq!(peek(next).expect("live").network, Network::None);
    }

    #[test]
    fn a_limit_at_or_below_zero_clears_the_bound_rather_than_setting_one() {
        let handle = gos_rt_sandbox_policy_max_memory(gos_rt_sandbox_policy_new(), 4096);
        assert_eq!(peek(handle).expect("live").resources.max_memory, Some(4096));
        let cleared = gos_rt_sandbox_policy_max_memory(handle, 0);
        assert_eq!(peek(cleared).expect("live").resources.max_memory, None);
    }

    /// A count past `u32` must answer the largest bound the field can
    /// hold, never a wrapped one: truncation would turn the biggest
    /// number a caller can write into the most restrictive limit.
    #[test]
    fn a_process_count_past_the_field_saturates_instead_of_wrapping() {
        let handle = gos_rt_sandbox_policy_max_processes(
            gos_rt_sandbox_policy_new(),
            i64::from(u32::MAX) + 1,
        );
        assert_eq!(
            peek(handle).expect("live").resources.max_processes,
            Some(u32::MAX)
        );
    }

    /// The verdict a caller matches on has exactly three spellings, and
    /// only a partial one carries a reason.
    #[test]
    fn an_enforcement_verdict_is_one_of_three_arms() {
        use gossamer_sandbox::Enforcement;
        assert_eq!(enforcement_kind(&Enforcement::Full), "full");
        assert_eq!(enforcement_kind(&Enforcement::None), "none");
        let partial = Enforcement::Partial("no cgroup".to_string());
        assert_eq!(enforcement_kind(&partial), "partial");
        assert_eq!(enforcement_reason(&partial), "no cgroup");
        assert_eq!(enforcement_reason(&Enforcement::Full), "");
    }
}
