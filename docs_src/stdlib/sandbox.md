# `std::sandbox`

Status: experimental

Run a command under an OS-native sandbox: one policy model, three backends, no daemon or root.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [policy model and the three backends](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-sandbox/src/lib.rs) live in their own crate, which depends on no other Gossamer crate: a sandbox exists to contain a build system, so it must not need one in order to build. The [language binding](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-runtime/src/c_abi/sandbox.rs) exposes that model unchanged.

A level the host cannot honor fails closed and names the blocking primitive; it is never quietly downgraded. An unknown enum spelling - a level, a network mode, a temp mode - leaves the setting as it was, so a typo can never weaken a policy.

A policy says what a command may reach. It carries no timeout, no memory cap, and no process count: bounding what a run uses is the caller's own business, and a limit two of the three backends could only partly apply would be a guarantee in name only.

### Building a policy

Every builder answers the policy as it now stands, so a chain reads as one expression.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| `Policy` | `type Policy` | What a command may reach. An opaque handle, as `fs::File` is. |
| `Policy::new` | `fn new() -> sandbox::Policy` | Nothing reachable, no network, a private temp, level `standard`. |
| `Policy::command_default` | `fn command_default(cwd: String) -> sandbox::Policy` | The working directory read-write, the network denied, credentials denied. |
| `read_write` / `read_only` / `deny` | `fn read_write(path: String) -> sandbox::Policy` | Grants or refuses a path and everything beneath it. An explicit allow outranks a deny of the same path; a deny beneath a grant wins by being the more specific rule. |
| `read_only_cwd` | `fn read_only_cwd() -> sandbox::Policy` | Downgrades the working directory to read-only. |
| `network_mode` | `fn network_mode(name: String) -> sandbox::Policy` | `none`, `client` (outbound only), or `open`. |
| `for_fetch_phase` | `fn for_fetch_phase() -> sandbox::Policy` | Outbound-only plus the resolver files a name lookup needs. |
| `env_allow` / `env_set` | `fn env_allow(name: String) -> sandbox::Policy` | The environment is an allowlist, not an addition. Loader variables are refused outright. |
| `temp` | `fn temp(mode: String) -> sandbox::Policy` | `private` (default, removed on exit) or `inherit`. |
| `level` | `fn level(name: String) -> sandbox::Policy` | `none`, `basic`, `standard`, or `strict`. |
| `working_directory` | `fn working_directory(path: String) -> sandbox::Policy` | Where the child starts. |

### Running a command

| Item | Canonical signature or declaration | Description |
|---|---|---|
| `run` | `fn run(policy: sandbox::Policy, argv: Vec<String>) -> Result<process::Output, errors::Error>` | Captures the child's output and answers the same `{ stdout, stderr, code }` shape `process::run` does. Blocks off the scheduler. |
| `run_inherit` | `fn run_inherit(policy: sandbox::Policy, argv: Vec<String>) -> i64` | Gives the child the caller's own streams and answers the exit-code contract. The wrapper-command shape. |
| `exit_policy_error` / `exit_command_not_found` / `exit_level_unavailable` / `exit_signal_base` | `fn exit_policy_error() -> i64` | The shared contract, so every consumer reports the same failure the same way. |

### Reading a policy back

| Item | Canonical signature or declaration | Description |
|---|---|---|
| `check` | `fn check() -> Result<(), errors::Error>` | What a run would refuse before anything is spawned. |
| `mechanisms` | `fn mechanisms() -> Vec<String>` | The enforcement a run installs, in the order it is applied. |
| `to_json` | `fn to_json() -> String` | The compiled policy as JSON. |
| `access` | `fn access(path: String) -> String` | `read-write`, `read-only`, or `deny` for that exact path. |
| `read_write_grants` / `read_only_grants` / `denials` | `fn denials() -> Vec<String>` | The compiled paths carrying each access. |
| `environment_names` / `environment_value` | `fn environment_names() -> Vec<String>` | Every name the child will actually see, and what it will see for one. |
| `level_name` / `network_name` / `working_directory_path` | `fn level_name() -> String` | What the policy asks for. |

### What the host will actually honor

A policy's request and a host's guarantee are different questions, so they are different calls. A consumer that reports the request as the guarantee tells an operator a denial is in force that the kernel never installed.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| `level_blocker` | `fn level_blocker() -> String` | What stops this host honoring the level, or the empty string. |
| `network_enforcement_kind` / `_reason` | `fn network_enforcement_kind() -> String` | How completely this run's network setting is enforced. |
| `max_level` / `platform` / `os_description` | `fn max_level() -> String` | The highest level this host can honor, which backend answers, and the machine. |
| `filesystem_kind` / `network_kind` / `process_isolation_kind` | `fn filesystem_kind() -> String` | Each dimension as an arm to match on: `full`, `partial`, or `none`. |
| `filesystem_reason` / `network_reason` / `process_isolation_reason` | `fn filesystem_reason() -> String` | What a partial verdict does not cover, or the empty string. |
| `notes` | `fn notes() -> Vec<String>` | Everything the scalar accessors cannot say: the Landlock ABI, the sysctl that blocks `strict`, whether loopback works inside an `AppContainer`. |
| `capabilities_json` | `fn capabilities_json() -> String` | The whole report, for a program that wants more than the accessors give it. |

### Discovery

| Item | Canonical signature or declaration | Description |
|---|---|---|
| `expand` | `fn expand(text: String) -> Option<String>` | A written path with `~` and environment references resolved. |
| `prefix_of` / `resolve_on_path` | `fn prefix_of(name: String) -> Option<String>` | The install prefix of a tool on `PATH`, and where `PATH` resolves a name. |
| `home_directory` | `fn home_directory() -> Option<String>` | The caller's home, as the presets resolve it. |
| `env_never_passed` | `fn env_never_passed(name: String) -> bool` | Whether a policy refuses to pass a name, because it redirects the loader or an interpreter's startup. |

A grant an interrupted Windows run leaves on a host ACL is revoked by the next run before it grants anything of its own, so there is no cleanup command to remember.
