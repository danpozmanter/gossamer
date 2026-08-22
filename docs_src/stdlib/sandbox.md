# `std::sandbox`

Status: experimental

Run a command under an OS-native sandbox: one policy model, three backends, no daemon or root.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [policy model and the three backends](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-sandbox/src/lib.rs) live in their own crate, which depends on no other Gossamer crate: a sandbox exists to contain a build system, so it must not need one in order to build. The [language binding](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-runtime/src/c_abi/sandbox.rs) exposes that model unchanged.

A level the host cannot honor fails closed and names the blocking primitive; it is never quietly downgraded. An unknown enum spelling - a level, a network mode, a temp mode - leaves the setting as it was, so a typo can never weaken a policy.

### Building a policy

Every builder answers the policy as it now stands, so a chain reads as one expression.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| `Policy` | `type Policy` | What a command may reach. An opaque handle, as `fs::File` is. |
| `Policy::new` | `fn new() -> sandbox::Policy` | Nothing reachable, no network, a private temp, level `standard`. |
| `Policy::build_default` | `fn build_default(root: String) -> sandbox::Policy` | The policy `gos build --sandbox` compiles under. |
| `Policy::command_default` | `fn command_default(cwd: String) -> sandbox::Policy` | The working directory read-write, the network denied, credentials denied. |
| `read_write` / `read_only` / `deny` | `fn read_write(path: String) -> sandbox::Policy` | Grants or refuses a path and everything beneath it. A denial beats a grant at equal specificity. |
| `network` | `fn network(allow: bool) -> sandbox::Policy` | The two-way form: open or denied. |
| `network_mode` | `fn network_mode(name: String) -> sandbox::Policy` | `none`, `client` (outbound only), or `open`. |
| `for_fetch_phase` | `fn for_fetch_phase() -> sandbox::Policy` | Outbound-only plus the resolver files a name lookup needs. |
| `env_allow` / `env_set` | `fn env_allow(name: String) -> sandbox::Policy` | The environment is an allowlist, not an addition. Loader variables are refused outright. |
| `temp` / `temp_path` | `fn temp(mode: String) -> sandbox::Policy` | `private` (default, removed on exit) or `inherit`; `temp_path` names one. |
| `timeout` | `fn timeout(ms: i64) -> sandbox::Policy` | Wall-clock bound on the whole process tree. |
| `max_processes` / `max_memory` / `max_cpu_time` / `max_file_size` / `max_temp_size` | `fn max_memory(bytes: i64) -> sandbox::Policy` | Resource bounds. A value at or below zero clears the bound. |
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
| `explain` | `fn explain() -> String` | The compiled policy and the mechanisms a run would install, as text. |
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
| `resource_enforcement_kind` / `_reason` | `fn resource_enforcement_kind() -> String` | Whether every limit the policy names will be applied. |
| `max_level` / `platform` / `os_description` | `fn max_level() -> String` | The highest level this host can honor, which backend answers, and the machine. |
| `filesystem` / `network_enforcement` / `process_isolation` / `resource_limits` | `fn filesystem() -> String` | Each dimension as a sentence: `full`, `partial (reason)`, or `none`. |
| `filesystem_kind` / `network_kind` / `process_isolation_kind` / `resource_limits_kind` | `fn filesystem_kind() -> String` | The same verdict as an arm to match on: `full`, `partial`, or `none`. |
| `filesystem_reason` / `network_reason` / `process_isolation_reason` / `resource_limits_reason` | `fn filesystem_reason() -> String` | What a partial verdict does not cover, or the empty string. |
| `notes` | `fn notes() -> Vec<String>` | Everything the scalar accessors cannot say: the Landlock ABI, the sysctl that blocks `strict`, whether loopback works inside an `AppContainer`. |
| `capabilities_json` | `fn capabilities_json() -> String` | The whole report, for a program that wants more than the accessors give it. |

### Discovery and host upkeep

| Item | Canonical signature or declaration | Description |
|---|---|---|
| `expand` | `fn expand(text: String) -> Option<String>` | A written path with `~` and environment references resolved. |
| `prefix_of` / `resolve_on_path` | `fn prefix_of(name: String) -> Option<String>` | The install prefix of a tool on `PATH`, and where `PATH` resolves a name. |
| `home_directory` | `fn home_directory() -> Option<String>` | The caller's home, as the presets resolve it. |
| `rust_toolchain_paths` | `fn rust_toolchain_paths() -> Vec<String>` | Every path a policy that has to run cargo must grant. |
| `stale_grant_count` / `clean_stale_grants` | `fn clean_stale_grants() -> Result<i64, errors::Error>` | The ACL grants an interrupted Windows run leaves behind. Zero everywhere else: no other backend reaches a path by editing its permissions. |
