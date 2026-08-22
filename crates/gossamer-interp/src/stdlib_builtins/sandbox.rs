//! `std::sandbox` builtins for the bytecode VM.
//!
//! The same `gossamer-sandbox` policy model `gos build --sandbox` uses,
//! so a Gossamer program can build that policy - or one of its own -
//! without reaching for a platform detail.
//!
//! A `sandbox::Policy` is an opaque handle, as `fs::File` and
//! `process::Child` are. Each builder call answers the policy as it now
//! stands, which is what lets a `|>` chain read as one expression.

use gossamer_sandbox::{Access, Enforcement, Level, Network, SandboxPolicy, Stdio, Temp};

use crate::builtins::{
    BuiltinFnPub, as_str, err_variant, none_variant, ok_variant, some_variant, value_to_int,
};
use crate::value::{RuntimeResult, Value};

use super::*;

/// Registers the module's free functions and the `Policy` methods.
pub(crate) fn install_sandbox(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        ("run", builtin_sandbox_run as BuiltinFnPub),
        ("max_level", builtin_sandbox_max_level),
        ("platform", builtin_sandbox_platform),
        ("filesystem", builtin_sandbox_filesystem),
        ("network_enforcement", builtin_sandbox_network_enforcement),
        ("process_isolation", builtin_sandbox_process_isolation),
        ("resource_limits", builtin_sandbox_resource_limits),
        ("notes", builtin_sandbox_notes),
        ("capabilities_json", builtin_sandbox_capabilities_json),
        ("os_description", builtin_sandbox_os_description),
        ("filesystem_kind", builtin_sandbox_filesystem_kind),
        ("filesystem_reason", builtin_sandbox_filesystem_reason),
        ("network_kind", builtin_sandbox_network_kind),
        ("network_reason", builtin_sandbox_network_reason),
        (
            "process_isolation_kind",
            builtin_sandbox_process_isolation_kind,
        ),
        (
            "process_isolation_reason",
            builtin_sandbox_process_isolation_reason,
        ),
        ("resource_limits_kind", builtin_sandbox_resource_limits_kind),
        (
            "resource_limits_reason",
            builtin_sandbox_resource_limits_reason,
        ),
        ("expand", builtin_sandbox_expand),
        ("prefix_of", builtin_sandbox_prefix_of),
        ("resolve_on_path", builtin_sandbox_resolve_on_path),
        ("home_directory", builtin_sandbox_home_directory),
        ("rust_toolchain_paths", builtin_sandbox_rust_toolchain_paths),
        ("stale_grant_count", builtin_sandbox_stale_grant_count),
        ("clean_stale_grants", builtin_sandbox_clean_stale_grants),
        ("exit_policy_error", builtin_sandbox_exit_policy_error),
        (
            "exit_command_not_found",
            builtin_sandbox_exit_command_not_found,
        ),
        (
            "exit_level_unavailable",
            builtin_sandbox_exit_level_unavailable,
        ),
        ("exit_signal_base", builtin_sandbox_exit_signal_base),
        ("run_inherit", builtin_sandbox_run_inherit),
    ] {
        let qualified: &'static str = Box::leak(format!("sandbox::{short}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, call)));
    }

    // Constructors are reached as `sandbox::Policy::new()` and the
    // methods as `policy.read_write(..)`, so each is registered under
    // its qualified path and under the bare method name the receiver
    // dispatch falls back to.
    let methods: &[(&str, BuiltinFnPub)] = &[
        ("Policy::new", builtin_policy_new),
        ("Policy::build_default", builtin_policy_build_default),
        ("Policy::command_default", builtin_policy_command_default),
        ("Policy::read_write", builtin_policy_read_write),
        ("Policy::read_only", builtin_policy_read_only),
        ("Policy::deny", builtin_policy_deny),
        ("Policy::network", builtin_policy_network),
        ("Policy::env_allow", builtin_policy_env_allow),
        ("Policy::env_set", builtin_policy_env_set),
        ("Policy::timeout", builtin_policy_timeout),
        ("Policy::level", builtin_policy_level),
        (
            "Policy::working_directory",
            builtin_policy_working_directory,
        ),
        ("Policy::explain", builtin_policy_explain),
        ("Policy::network_mode", builtin_policy_network_mode),
        ("Policy::for_fetch_phase", builtin_policy_for_fetch_phase),
        ("Policy::read_only_cwd", builtin_policy_read_only_cwd),
        ("Policy::temp", builtin_policy_temp),
        ("Policy::temp_path", builtin_policy_temp_path),
        ("Policy::max_processes", builtin_policy_max_processes),
        ("Policy::max_memory", builtin_policy_max_memory),
        ("Policy::max_cpu_time", builtin_policy_max_cpu_time),
        ("Policy::max_file_size", builtin_policy_max_file_size),
        ("Policy::max_temp_size", builtin_policy_max_temp_size),
        ("Policy::check", builtin_policy_check),
        ("Policy::mechanisms", builtin_policy_mechanisms),
        ("Policy::to_json", builtin_policy_to_json),
        ("Policy::access", builtin_policy_access),
        (
            "Policy::read_write_grants",
            builtin_policy_read_write_grants,
        ),
        ("Policy::read_only_grants", builtin_policy_read_only_grants),
        ("Policy::denials", builtin_policy_denials),
        (
            "Policy::environment_names",
            builtin_policy_environment_names,
        ),
        (
            "Policy::environment_value",
            builtin_policy_environment_value,
        ),
        ("Policy::level_name", builtin_policy_level_name),
        ("Policy::network_name", builtin_policy_network_name),
        (
            "Policy::working_directory_path",
            builtin_policy_working_directory_path,
        ),
        ("Policy::level_blocker", builtin_policy_level_blocker),
        (
            "Policy::network_enforcement_kind",
            builtin_policy_network_enforcement_kind,
        ),
        (
            "Policy::network_enforcement_reason",
            builtin_policy_network_enforcement_reason,
        ),
        (
            "Policy::resource_enforcement_kind",
            builtin_policy_resource_enforcement_kind,
        ),
        (
            "Policy::resource_enforcement_reason",
            builtin_policy_resource_enforcement_reason,
        ),
    ];
    for (short, call) in methods {
        let qualified: &'static str = Box::leak(format!("sandbox::{short}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, *call)));
        let bare: &'static str = Box::leak((*short).to_string().into_boxed_str());
        globals.push((bare, crate::builtins::builtin_pub(bare, *call)));
    }
}

static NEXT_POLICY_HANDLE: GlobalReg<i64> =
    GlobalReg::new(|| parking_lot::ReentrantMutex::new(RefCell::new(1)));
static POLICY_REGISTRY: GlobalReg<StdHashMap<i64, SandboxPolicy>> =
    GlobalReg::new(|| parking_lot::ReentrantMutex::new(RefCell::new(StdHashMap::new())));

fn store(policy: SandboxPolicy) -> Value {
    let id = NEXT_POLICY_HANDLE.with(|cell| {
        let mut next = cell.borrow_mut();
        let id = *next;
        *next += 1;
        id
    });
    POLICY_REGISTRY.with(|registry| registry.borrow_mut().insert(id, policy));
    handle_struct("sandbox::Policy", id)
}

fn load(value: Option<&Value>) -> Option<SandboxPolicy> {
    let id = handle_id(value?)?;
    POLICY_REGISTRY.with(|registry| registry.borrow().get(&id).cloned())
}

/// Applies `edit` to the policy the first argument names.
fn edited(
    args: &[Value],
    edit: impl FnOnce(SandboxPolicy) -> SandboxPolicy,
) -> RuntimeResult<Value> {
    match load(args.first()) {
        Some(policy) => Ok(store(edit(policy))),
        None => Ok(err_variant("sandbox: the receiver is not a policy")),
    }
}

fn builtin_policy_new(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(store(SandboxPolicy::new()))
}

fn builtin_policy_build_default(args: &[Value]) -> RuntimeResult<Value> {
    let root = std::path::PathBuf::from(args.first().and_then(as_str).unwrap_or("."));
    let caches = cache_roots();
    let toolchain = toolchain_roots();
    Ok(store(SandboxPolicy::build_default(
        &root, &caches, &toolchain,
    )))
}

fn builtin_policy_command_default(args: &[Value]) -> RuntimeResult<Value> {
    let cwd = std::path::PathBuf::from(args.first().and_then(as_str).unwrap_or("."));
    Ok(store(SandboxPolicy::command_default(&cwd)))
}

fn cache_roots() -> Vec<std::path::PathBuf> {
    gossamer_sandbox::home_directory().map_or_else(Vec::new, |home| {
        vec![
            home.join(".gossamer").join("cache"),
            home.join(".cargo").join("registry"),
            home.join(".cargo").join("git"),
        ]
    })
}

fn toolchain_roots() -> Vec<std::path::PathBuf> {
    gossamer_sandbox::discover::rust_toolchain_paths()
}

fn builtin_policy_read_write(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.get(1).and_then(as_str).unwrap_or("").to_string();
    edited(args, |policy| policy.read_write(path))
}

fn builtin_policy_read_only(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.get(1).and_then(as_str).unwrap_or("").to_string();
    edited(args, |policy| policy.read_only(path))
}

fn builtin_policy_deny(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.get(1).and_then(as_str).unwrap_or("").to_string();
    edited(args, |policy| policy.deny(path))
}

fn builtin_policy_network(args: &[Value]) -> RuntimeResult<Value> {
    let allow = matches!(args.get(1), Some(Value::Bool(true)));
    edited(args, |policy| {
        policy.network(if allow { Network::Open } else { Network::None })
    })
}

fn builtin_policy_env_allow(args: &[Value]) -> RuntimeResult<Value> {
    let name = args.get(1).and_then(as_str).unwrap_or("").to_string();
    edited(args, |policy| policy.env_allow([name]))
}

fn builtin_policy_env_set(args: &[Value]) -> RuntimeResult<Value> {
    let name = args.get(1).and_then(as_str).unwrap_or("").to_string();
    let value = args.get(2).and_then(as_str).unwrap_or("").to_string();
    edited(args, |policy| policy.env_set(name, value))
}

fn builtin_policy_timeout(args: &[Value]) -> RuntimeResult<Value> {
    let milliseconds = args.get(1).and_then(value_to_int).unwrap_or(0).max(0);
    edited(args, |policy| {
        policy.timeout(std::time::Duration::from_millis(milliseconds as u64))
    })
}

fn builtin_policy_level(args: &[Value]) -> RuntimeResult<Value> {
    // An unknown name leaves the level as it was, so a typo never
    // silently weakens a policy.
    let requested = args.get(1).and_then(as_str).and_then(Level::parse);
    edited(args, |policy| match requested {
        Some(level) => policy.level(level),
        None => policy,
    })
}

fn builtin_policy_working_directory(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.get(1).and_then(as_str).unwrap_or("").to_string();
    edited(args, |policy| policy.working_directory(path))
}

fn builtin_policy_explain(args: &[Value]) -> RuntimeResult<Value> {
    let Some(policy) = load(args.first()) else {
        return Ok(Value::String(
            "sandbox: the receiver is not a policy".into(),
        ));
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
    Ok(Value::String(text.into()))
}

fn builtin_sandbox_run(args: &[Value]) -> RuntimeResult<Value> {
    let Some(policy) = load(args.first()) else {
        return Ok(err_variant(
            "sandbox::run: the first argument is not a policy",
        ));
    };
    let argv: Vec<String> = match args.get(1) {
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| as_str(item).unwrap_or("").to_string())
            .collect(),
        _ => Vec::new(),
    };
    if argv.is_empty() {
        return Ok(err_variant("sandbox::run: no command to run"));
    }
    let sandbox = match gossamer_sandbox::Sandbox::new(&policy) {
        Ok(sandbox) => sandbox,
        Err(error) => return Ok(err_variant(error.to_string())),
    };
    match gossamer_runtime::sched_global::run_blocking("sandbox-run", move || {
        sandbox.run_with(&argv, Stdio::Capture)
    }) {
        Ok(Ok(output)) => Ok(ok_variant(Value::struct_(
            "Output",
            vec![
                ("stdout", Value::String(output.stdout_text().into())),
                ("stderr", Value::String(output.stderr_text().into())),
                ("code", Value::Int(i64::from(output.code))),
            ],
        ))),
        Ok(Err(error)) => Ok(err_variant(error.to_string())),
        Err(error) => Ok(err_variant(format!("sandbox::run: {error}"))),
    }
}

fn builtin_sandbox_max_level(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(
        gossamer_sandbox::capabilities().max_level.as_str().into(),
    ))
}

fn builtin_sandbox_platform(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(
        gossamer_sandbox::capabilities().platform.to_string().into(),
    ))
}

fn builtin_sandbox_filesystem(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(
        gossamer_sandbox::capabilities()
            .filesystem
            .to_string()
            .into(),
    ))
}

fn builtin_sandbox_network_enforcement(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(
        gossamer_sandbox::capabilities().network.to_string().into(),
    ))
}

fn builtin_sandbox_process_isolation(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(
        gossamer_sandbox::capabilities()
            .process_isolation
            .to_string()
            .into(),
    ))
}

fn builtin_sandbox_resource_limits(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(
        gossamer_sandbox::capabilities()
            .resource_limits
            .to_string()
            .into(),
    ))
}

fn builtin_sandbox_notes(_args: &[Value]) -> RuntimeResult<Value> {
    let notes: Vec<Value> = gossamer_sandbox::capabilities()
        .notes
        .into_iter()
        .map(|note| Value::String(note.into()))
        .collect();
    Ok(Value::Array(Arc::new(notes)))
}

fn builtin_sandbox_capabilities_json(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(
        gossamer_sandbox::capabilities().to_json().into(),
    ))
}

// ---------------------------------------------------------------------
// Policy builders the first cut left out. Each edits a field the
// library already exposes, so the binding widens what a caller can say
// without changing what any backend enforces.
// ---------------------------------------------------------------------

/// The `Network` mode `name` spells, or `None` when it spells nothing.
fn parse_network(name: &str) -> Option<Network> {
    match name {
        "none" => Some(Network::None),
        "client" => Some(Network::Client),
        "open" => Some(Network::Open),
        _ => None,
    }
}

fn builtin_policy_network_mode(args: &[Value]) -> RuntimeResult<Value> {
    // An unknown name leaves the setting alone: a typo must never open
    // a network the policy meant to close.
    let requested = args.get(1).and_then(as_str).and_then(parse_network);
    edited(args, |policy| match requested {
        Some(network) => policy.network(network),
        None => policy,
    })
}

fn builtin_policy_for_fetch_phase(args: &[Value]) -> RuntimeResult<Value> {
    edited(args, SandboxPolicy::for_fetch_phase)
}

fn builtin_policy_read_only_cwd(args: &[Value]) -> RuntimeResult<Value> {
    edited(args, SandboxPolicy::read_only_cwd)
}

fn builtin_policy_temp(args: &[Value]) -> RuntimeResult<Value> {
    let requested = match args.get(1).and_then(as_str) {
        Some("private") => Some(Temp::Private),
        Some("inherit") => Some(Temp::Inherit),
        _ => None,
    };
    edited(args, |policy| match requested {
        Some(temp) => policy.temp(temp),
        None => policy,
    })
}

fn builtin_policy_temp_path(args: &[Value]) -> RuntimeResult<Value> {
    let path = std::path::PathBuf::from(args.get(1).and_then(as_str).unwrap_or(""));
    edited(args, |policy| policy.temp(Temp::Path(path)))
}

/// The limit `args[1]` asks for, or `None` when it asks for none.
///
/// Zero and below clear the limit rather than setting one nothing can
/// satisfy: a caller that computed a bound and got nothing means
/// unbounded.
fn limit(args: &[Value]) -> Option<u64> {
    let value = args.get(1).and_then(value_to_int).unwrap_or(0);
    if value > 0 { Some(value as u64) } else { None }
}

fn builtin_policy_max_processes(args: &[Value]) -> RuntimeResult<Value> {
    // Saturating rather than truncating: a count past `u32` would wrap
    // to a tiny one, turning the largest bound a caller can write into
    // the most restrictive one.
    let count = limit(args).map(|value| u32::try_from(value).unwrap_or(u32::MAX));
    edited(args, move |mut policy| {
        policy.resources.max_processes = count;
        policy
    })
}

fn builtin_policy_max_memory(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = limit(args);
    edited(args, move |mut policy| {
        policy.resources.max_memory = bytes;
        policy
    })
}

fn builtin_policy_max_file_size(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = limit(args);
    edited(args, move |mut policy| {
        policy.resources.max_file_size = bytes;
        policy
    })
}

fn builtin_policy_max_temp_size(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = limit(args);
    edited(args, move |mut policy| {
        policy.resources.max_temp_size = bytes;
        policy
    })
}

fn builtin_policy_max_cpu_time(args: &[Value]) -> RuntimeResult<Value> {
    let span = limit(args).map(std::time::Duration::from_millis);
    edited(args, move |mut policy| {
        policy.resources.max_cpu_time = span;
        policy
    })
}

// ---------------------------------------------------------------------
// Introspection: what the policy says, and what the host will honor.
// ---------------------------------------------------------------------

/// The compiled form of the policy `args[0]` names.
fn compiled(args: &[Value]) -> Option<gossamer_sandbox::CompiledPolicy> {
    load(args.first())?.compile().ok()
}

/// The sandbox `args[0]`'s policy would build, when the host can honor
/// it.
fn sandbox_of(args: &[Value]) -> Option<gossamer_sandbox::Sandbox> {
    gossamer_sandbox::Sandbox::new(&load(args.first())?).ok()
}

fn text_value(text: impl Into<String>) -> RuntimeResult<Value> {
    Ok(Value::String(text.into().into()))
}

fn string_list(values: Vec<String>) -> RuntimeResult<Value> {
    Ok(Value::Array(Arc::new(
        values
            .into_iter()
            .map(|value| Value::String(value.into()))
            .collect(),
    )))
}

fn builtin_policy_check(args: &[Value]) -> RuntimeResult<Value> {
    let Some(policy) = load(args.first()) else {
        return Ok(err_variant("sandbox: the receiver is not a policy"));
    };
    match gossamer_sandbox::Sandbox::new(&policy) {
        Ok(_) => Ok(ok_variant(Value::Unit)),
        Err(error) => Ok(err_variant(error.to_string())),
    }
}

fn builtin_policy_mechanisms(args: &[Value]) -> RuntimeResult<Value> {
    string_list(sandbox_of(args).map_or_else(Vec::new, |sandbox| sandbox.mechanisms()))
}

/// A policy that will not compile has no compiled form to serialize, so
/// this answers the empty string rather than the reason as bare text:
/// the call promises JSON, and a caller that parses the answer must not
/// have to guess whether it got any. `check` is what carries the reason.
fn builtin_policy_to_json(args: &[Value]) -> RuntimeResult<Value> {
    text_value(compiled(args).map_or_else(String::new, |policy| policy.to_json()))
}

/// The spelling of `access` a Gossamer caller matches on.
const fn access_name(access: Access) -> &'static str {
    match access {
        Access::ReadOnly => "read-only",
        Access::ReadWrite => "read-write",
        Access::Deny => "deny",
    }
}

fn builtin_policy_access(args: &[Value]) -> RuntimeResult<Value> {
    let path = std::path::PathBuf::from(args.get(1).and_then(as_str).unwrap_or(""));
    text_value(compiled(args).map_or("deny", |policy| access_name(policy.access(&path))))
}

/// The compiled paths carrying exactly `access`.
///
/// One list per access rather than one list of pairs: the access is
/// then the name of the call, and a caller never has to agree with the
/// binding about how a pair was spelled.
fn rules(args: &[Value], access: Access) -> Vec<String> {
    let Some(policy) = compiled(args) else {
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

fn builtin_policy_read_write_grants(args: &[Value]) -> RuntimeResult<Value> {
    string_list(rules(args, Access::ReadWrite))
}

fn builtin_policy_read_only_grants(args: &[Value]) -> RuntimeResult<Value> {
    string_list(rules(args, Access::ReadOnly))
}

fn builtin_policy_denials(args: &[Value]) -> RuntimeResult<Value> {
    string_list(rules(args, Access::Deny))
}

fn builtin_policy_environment_names(args: &[Value]) -> RuntimeResult<Value> {
    string_list(compiled(args).map_or_else(Vec::new, |policy| {
        policy.environment().into_keys().collect()
    }))
}

fn builtin_policy_environment_value(args: &[Value]) -> RuntimeResult<Value> {
    let name = args.get(1).and_then(as_str).unwrap_or("").to_string();
    text_value(compiled(args).map_or_else(String::new, |policy| {
        policy.environment().remove(&name).unwrap_or_default()
    }))
}

fn builtin_policy_level_name(args: &[Value]) -> RuntimeResult<Value> {
    text_value(
        load(args.first())
            .map_or(Level::None, |policy| policy.level)
            .as_str(),
    )
}

/// The spelling of `network` a Gossamer caller matches on.
const fn network_name(network: Network) -> &'static str {
    match network {
        Network::None => "none",
        Network::Client => "client",
        Network::Open => "open",
    }
}

fn builtin_policy_network_name(args: &[Value]) -> RuntimeResult<Value> {
    text_value(network_name(
        load(args.first()).map_or(Network::None, |policy| policy.network),
    ))
}

fn builtin_policy_working_directory_path(args: &[Value]) -> RuntimeResult<Value> {
    text_value(
        load(args.first())
            .and_then(|policy| policy.working_directory)
            .map_or_else(String::new, |path| path.display().to_string()),
    )
}

/// The other half of the level contract: `sandbox::max_level()` says
/// what the host can do, this says what stands in the way of the level
/// the policy asked for.
fn builtin_policy_level_blocker(args: &[Value]) -> RuntimeResult<Value> {
    let Some(policy) = load(args.first()) else {
        return text_value("sandbox: the receiver is not a policy");
    };
    text_value(match gossamer_sandbox::Sandbox::new(&policy) {
        Err(gossamer_sandbox::SandboxError::LevelUnavailable { reason, .. }) => reason,
        _ => String::new(),
    })
}

/// `full`, `partial`, or `none` - the arm of an `Enforcement`, for a
/// caller that matches on the verdict rather than printing it.
const fn enforcement_kind(enforcement: &Enforcement) -> &'static str {
    match enforcement {
        Enforcement::Full => "full",
        Enforcement::Partial(_) => "partial",
        Enforcement::None => "none",
    }
}

/// What a `partial` verdict does not cover, or the empty string when
/// the verdict carries no reason.
fn enforcement_reason(enforcement: &Enforcement) -> String {
    match enforcement {
        Enforcement::Partial(reason) => reason.clone(),
        _ => String::new(),
    }
}

fn policy_network_enforcement(args: &[Value]) -> Enforcement {
    sandbox_of(args).map_or(Enforcement::None, |sandbox| sandbox.network_enforcement())
}

fn builtin_policy_network_enforcement_kind(args: &[Value]) -> RuntimeResult<Value> {
    text_value(enforcement_kind(&policy_network_enforcement(args)))
}

fn builtin_policy_network_enforcement_reason(args: &[Value]) -> RuntimeResult<Value> {
    text_value(enforcement_reason(&policy_network_enforcement(args)))
}

fn policy_resource_enforcement(args: &[Value]) -> Enforcement {
    load(args.first()).map_or(Enforcement::None, |policy| {
        gossamer_sandbox::resource_enforcement(&policy.resources, policy.level)
    })
}

fn builtin_policy_resource_enforcement_kind(args: &[Value]) -> RuntimeResult<Value> {
    text_value(enforcement_kind(&policy_resource_enforcement(args)))
}

fn builtin_policy_resource_enforcement_reason(args: &[Value]) -> RuntimeResult<Value> {
    text_value(enforcement_reason(&policy_resource_enforcement(args)))
}

// ---------------------------------------------------------------------
// The host report, beyond the scalars the first cut exposed.
// ---------------------------------------------------------------------

fn builtin_sandbox_os_description(_args: &[Value]) -> RuntimeResult<Value> {
    text_value(gossamer_sandbox::capabilities().os_description)
}

fn builtin_sandbox_filesystem_kind(_args: &[Value]) -> RuntimeResult<Value> {
    text_value(enforcement_kind(
        &gossamer_sandbox::capabilities().filesystem,
    ))
}

fn builtin_sandbox_filesystem_reason(_args: &[Value]) -> RuntimeResult<Value> {
    text_value(enforcement_reason(
        &gossamer_sandbox::capabilities().filesystem,
    ))
}

fn builtin_sandbox_network_kind(_args: &[Value]) -> RuntimeResult<Value> {
    text_value(enforcement_kind(&gossamer_sandbox::capabilities().network))
}

fn builtin_sandbox_network_reason(_args: &[Value]) -> RuntimeResult<Value> {
    text_value(enforcement_reason(
        &gossamer_sandbox::capabilities().network,
    ))
}

fn builtin_sandbox_process_isolation_kind(_args: &[Value]) -> RuntimeResult<Value> {
    text_value(enforcement_kind(
        &gossamer_sandbox::capabilities().process_isolation,
    ))
}

fn builtin_sandbox_process_isolation_reason(_args: &[Value]) -> RuntimeResult<Value> {
    text_value(enforcement_reason(
        &gossamer_sandbox::capabilities().process_isolation,
    ))
}

fn builtin_sandbox_resource_limits_kind(_args: &[Value]) -> RuntimeResult<Value> {
    text_value(enforcement_kind(
        &gossamer_sandbox::capabilities().resource_limits,
    ))
}

fn builtin_sandbox_resource_limits_reason(_args: &[Value]) -> RuntimeResult<Value> {
    text_value(enforcement_reason(
        &gossamer_sandbox::capabilities().resource_limits,
    ))
}

// ---------------------------------------------------------------------
// Discovery, so a profile can name a path the way an operator writes it.
// ---------------------------------------------------------------------

fn optional_path(path: Option<std::path::PathBuf>) -> RuntimeResult<Value> {
    Ok(match path {
        Some(path) => some_variant(Value::String(path.display().to_string().into())),
        None => none_variant(),
    })
}

fn builtin_sandbox_expand(args: &[Value]) -> RuntimeResult<Value> {
    optional_path(gossamer_sandbox::discover::expand(
        args.first().and_then(as_str).unwrap_or(""),
    ))
}

fn builtin_sandbox_prefix_of(args: &[Value]) -> RuntimeResult<Value> {
    optional_path(gossamer_sandbox::discover::prefix_of(
        args.first().and_then(as_str).unwrap_or(""),
    ))
}

fn builtin_sandbox_resolve_on_path(args: &[Value]) -> RuntimeResult<Value> {
    optional_path(gossamer_sandbox::discover::resolve_on_path(
        args.first().and_then(as_str).unwrap_or(""),
    ))
}

fn builtin_sandbox_home_directory(_args: &[Value]) -> RuntimeResult<Value> {
    optional_path(gossamer_sandbox::home_directory())
}

fn builtin_sandbox_rust_toolchain_paths(_args: &[Value]) -> RuntimeResult<Value> {
    string_list(
        toolchain_roots()
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
    )
}

// ---------------------------------------------------------------------
// Host maintenance and the exit-code contract.
// ---------------------------------------------------------------------

/// Windows grants a path to the container by writing its ACL, so an
/// interrupted run can leave one behind. No other backend edits a
/// path's permissions, so no other backend has anything to clean.
#[cfg(windows)]
fn stale_grants() -> usize {
    gossamer_sandbox::windows::stale_grant_count()
}

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

fn builtin_sandbox_stale_grant_count(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(stale_grants() as i64))
}

fn builtin_sandbox_clean_stale_grants(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(match clean_grants() {
        Ok(count) => ok_variant(Value::Int(count as i64)),
        Err(reason) => err_variant(reason),
    })
}

fn builtin_sandbox_exit_policy_error(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(i64::from(gossamer_sandbox::EXIT_POLICY_ERROR)))
}

fn builtin_sandbox_exit_command_not_found(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(i64::from(
        gossamer_sandbox::EXIT_COMMAND_NOT_FOUND,
    )))
}

fn builtin_sandbox_exit_level_unavailable(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(i64::from(
        gossamer_sandbox::EXIT_LEVEL_UNAVAILABLE,
    )))
}

fn builtin_sandbox_exit_signal_base(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(i64::from(gossamer_sandbox::EXIT_SIGNAL_BASE)))
}

/// `sandbox::run_inherit(policy, argv)` - runs `argv` with the caller's
/// own streams and answers the exit code the contract gives.
///
/// The wrapper shape: the child writes straight to the terminal, and
/// every failure between the policy and the finished child maps to its
/// own code, so a policy mistake is never mistaken for a program that
/// merely failed.
fn builtin_sandbox_run_inherit(args: &[Value]) -> RuntimeResult<Value> {
    let policy_error = Value::Int(i64::from(gossamer_sandbox::EXIT_POLICY_ERROR));
    let Some(policy) = load(args.first()) else {
        return Ok(policy_error);
    };
    let argv: Vec<String> = match args.get(1) {
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| as_str(item).unwrap_or("").to_string())
            .collect(),
        _ => Vec::new(),
    };
    if argv.is_empty() {
        return Ok(policy_error);
    }
    let sandbox = match gossamer_sandbox::Sandbox::new(&policy) {
        Ok(sandbox) => sandbox,
        Err(error) => return Ok(Value::Int(i64::from(error.exit_code()))),
    };
    // The child writes to the same terminal this program buffers into,
    // so whatever the wrapper has already said has to be on the wire
    // before the child says anything.
    gossamer_runtime::c_abi::print::flush_stdout_buffer();
    let outcome = gossamer_runtime::sched_global::run_blocking("sandbox-run", move || {
        sandbox.run_with(&argv, Stdio::Inherit)
    });
    Ok(match outcome {
        Ok(outcome) => Value::Int(i64::from(gossamer_sandbox::exit_code_for(&outcome))),
        Err(_) => policy_error,
    })
}

#[cfg(test)]
mod sandbox_builtin_tests {
    use super::*;

    #[test]
    fn a_builder_chain_answers_a_live_policy_each_step() {
        let policy = builtin_policy_new(&[]).expect("new");
        let with_network = builtin_policy_network(&[policy, Value::Bool(true)]).expect("network");
        let loaded = load(Some(&with_network)).expect("live policy");
        assert_eq!(loaded.network, Network::Open);
    }

    #[test]
    fn an_unknown_level_name_never_weakens_the_policy() {
        let policy = builtin_policy_new(&[]).expect("new");
        let same =
            builtin_policy_level(&[policy, Value::String("paranoid".into())]).expect("level");
        assert_eq!(load(Some(&same)).expect("live").level, Level::Standard);
    }

    #[test]
    fn an_unknown_network_mode_never_opens_a_closed_network() {
        let opened = builtin_policy_network_mode(&[
            builtin_policy_new(&[]).expect("new"),
            Value::String("open".into()),
        ])
        .expect("open");
        let after =
            builtin_policy_network_mode(&[opened, Value::String("opne".into())]).expect("typo");
        assert_eq!(load(Some(&after)).expect("live").network, Network::Open);

        let closed = builtin_policy_network_mode(&[
            builtin_policy_new(&[]).expect("new"),
            Value::String("opne".into()),
        ])
        .expect("typo");
        assert_eq!(load(Some(&closed)).expect("live").network, Network::None);
    }

    #[test]
    fn a_limit_at_or_below_zero_clears_the_bound_rather_than_setting_one() {
        let bounded =
            builtin_policy_max_memory(&[builtin_policy_new(&[]).expect("new"), Value::Int(4096)])
                .expect("bound");
        assert_eq!(
            load(Some(&bounded)).expect("live").resources.max_memory,
            Some(4096)
        );
        let cleared = builtin_policy_max_memory(&[bounded, Value::Int(0)]).expect("clear");
        assert_eq!(
            load(Some(&cleared)).expect("live").resources.max_memory,
            None
        );
    }

    /// A count past `u32` must answer the largest bound the field can
    /// hold, never a wrapped one: truncation would turn the biggest
    /// number a caller can write into the most restrictive limit.
    #[test]
    fn a_process_count_past_the_field_saturates_instead_of_wrapping() {
        let handle = builtin_policy_max_processes(&[
            builtin_policy_new(&[]).expect("new"),
            Value::Int(i64::from(u32::MAX) + 1),
        ])
        .expect("bound");
        assert_eq!(
            load(Some(&handle)).expect("live").resources.max_processes,
            Some(u32::MAX)
        );
    }

    /// The verdict a caller matches on has exactly three spellings, and
    /// only a partial one carries a reason.
    #[test]
    fn an_enforcement_verdict_is_one_of_three_arms() {
        assert_eq!(enforcement_kind(&Enforcement::Full), "full");
        assert_eq!(enforcement_kind(&Enforcement::None), "none");
        let partial = Enforcement::Partial("no cgroup".to_string());
        assert_eq!(enforcement_kind(&partial), "partial");
        assert_eq!(enforcement_reason(&partial), "no cgroup");
        assert_eq!(enforcement_reason(&Enforcement::Full), "");
    }

    /// A reader answers the compiled policy, so the access a path gets
    /// is the verdict a run would apply rather than the rule as written.
    #[test]
    fn a_reader_answers_the_compiled_policy() {
        let policy = builtin_policy_read_write(&[
            builtin_policy_new(&[]).expect("new"),
            Value::String(".".into()),
        ])
        .expect("grant");
        let verdict =
            builtin_policy_access(&[policy.clone(), Value::String(".".into())]).expect("access");
        assert_eq!(as_str(&verdict), Some("read-write"));
        let outside = builtin_policy_access(&[policy, Value::String("/".into())]).expect("access");
        assert_eq!(as_str(&outside), Some("deny"));
    }

    #[test]
    fn the_capability_accessors_answer_the_same_report_the_library_does() {
        let host = gossamer_sandbox::capabilities();
        let max = builtin_sandbox_max_level(&[]).expect("max_level");
        assert_eq!(as_str(&max), Some(host.max_level.as_str()));
    }
}
