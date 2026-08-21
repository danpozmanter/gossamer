//! `std::sandbox` builtins for the bytecode VM.
//!
//! The same `gossamer-sandbox` policy model `gos build --sandbox` uses,
//! so a Gossamer program can build that policy - or one of its own -
//! without reaching for a platform detail.
//!
//! A `sandbox::Policy` is an opaque handle, as `fs::File` and
//! `process::Child` are. Each builder call answers the policy as it now
//! stands, which is what lets a `|>` chain read as one expression.

use gossamer_sandbox::{Level, Network, SandboxPolicy, Stdio};

use crate::builtins::{BuiltinFnPub, as_str, err_variant, ok_variant, value_to_int};
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
    gossamer_sandbox::discover::query("rustc", &["--print", "sysroot"])
        .map(std::path::PathBuf::from)
        .into_iter()
        .collect()
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
    fn the_capability_accessors_answer_the_same_report_the_library_does() {
        let host = gossamer_sandbox::capabilities();
        let max = builtin_sandbox_max_level(&[]).expect("max_level");
        assert_eq!(as_str(&max), Some(host.max_level.as_str()));
    }
}
