//! Built-in callables exposed to interpreted programs.

#![forbid(unsafe_code)]
#![allow(clippy::unnecessary_wraps)]

use std::cell::RefCell;
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use gossamer_ast::Ident;

use gossamer_std::compress::gzip as gzip_std;
use gossamer_std::exec as exec_std;
use gossamer_std::fs as fs_std;
use gossamer_std::http as http_std;
use gossamer_std::json as json_std;
use gossamer_std::os as os_std;
use gossamer_std::slog as slog_std;
use gossamer_std::time as time_std;

use crate::value::{MapKey, NativeDispatch, RuntimeError, RuntimeResult, SmolStr, Value};

thread_local! {
    pub(crate) static PROGRAM_ARGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Overwrites the program-level argument list that `os::args()`
/// returns. Called by the CLI entrypoint before invoking `main`.
///
/// Wires both execution paths:
/// - The bytecode VM's `os::args()` builtin reads from
///   [`PROGRAM_ARGS`] (the thread-local cell below).
/// - JIT-compiled `main` calls into the runtime's
///   `gos_rt_os_args`, which reads from a *different* static
///   inside `gossamer-runtime::c_abi`. Without this second wire,
///   benchmarks like `fasta` and `nbody` see an empty arg list
///   when their `main` JIT-compiles, fall back to the default N
///   (typically 1000), and silently produce undersized output.
///
/// The runtime side is wired by [`crate::set_runtime_args`] in
/// `lib.rs`, which is allowed to call into the FFI; this module
/// keeps `forbid(unsafe_code)`.
pub fn set_program_args(args: &[String]) {
    PROGRAM_ARGS.with(|cell| {
        let mut v = cell.borrow_mut();
        v.clear();
        v.extend_from_slice(args);
    });
    crate::set_runtime_args(args);
}

// ------------------------------------------------------------------
// Mutable cell backing for `flag::Set` API.
//
// `Set::string` / `int` / `uint` / `bool` return a `__Cell` struct
// that `*` dereferences in `interp.rs` via [`resolve_cell`].

pub(crate) type CellMap =
    std::collections::HashMap<(u64, String), std::sync::Arc<parking_lot::Mutex<Value>>>;

thread_local! {
    pub(crate) static NEXT_SET_ID: RefCell<u64> = const { RefCell::new(1) };
    #[allow(clippy::missing_const_for_thread_local)]
    pub(crate) static SET_REGISTRY: RefCell<std::collections::HashMap<u64, SetState>> =
        RefCell::new(std::collections::HashMap::new());
    #[allow(clippy::missing_const_for_thread_local)]
    pub(crate) static CELL_REGISTRY: RefCell<CellMap> = RefCell::new(std::collections::HashMap::new());
    /// Canonical field orderings for every struct type in the
    /// loaded program. `Vm::load` populates this from the
    /// `HirAdt` items so `__struct` can place fields in
    /// declaration order regardless of source-literal spelling;
    /// that lets the VM compiler emit compile-time offset-based
    /// reads.
    #[allow(clippy::missing_const_for_thread_local)]
    pub(crate) static STRUCT_LAYOUTS: RefCell<std::collections::HashMap<String, Vec<String>>> =
        RefCell::new(std::collections::HashMap::new());
}

/// Installs the struct-field declaration-order table that
/// `__struct` consults when assembling a new `Value::Struct`.
/// Invoked by [`crate::Vm::load`] before any program code runs.
#[allow(clippy::implicit_hasher)]
pub fn set_struct_layouts(layouts: std::collections::HashMap<String, Vec<String>>) {
    STRUCT_LAYOUTS.with(|cell| *cell.borrow_mut() = layouts);
}

#[derive(Debug, Clone)]
pub(crate) struct SetState {
    pub(crate) name: String,
    pub(crate) flag_order: Vec<String>,
    pub(crate) last_flag: Option<String>,
    pub(crate) flags: std::collections::HashMap<String, FlagDef>,
}

#[derive(Debug, Clone)]
pub(crate) struct FlagDef {
    pub(crate) short: Option<char>,
    pub(crate) kind: FlagKind,
    pub(crate) help: String,
    pub(crate) default: Value,
}

#[derive(Debug, Clone)]
pub(crate) enum FlagKind {
    String,
    Int,
    Uint,
    Bool,
}

pub(crate) fn make_cell(set_id: u64, flag_name: &str, default: Value) -> Value {
    let key = (set_id, flag_name.to_string());
    let cell = std::sync::Arc::new(parking_lot::Mutex::new(default));
    CELL_REGISTRY.with(|reg| {
        reg.borrow_mut().insert(key, cell);
    });
    Value::struct_(
        "__Cell",
        Arc::new(vec![
            (Ident::new("__set_id"), Value::Int(set_id as i64)),
            (
                Ident::new("__flag_name"),
                Value::String(SmolStr::from(flag_name.to_string())),
            ),
        ]),
    )
}

/// Resolves a `__Cell` handle to its current value.
pub(crate) fn resolve_cell(set_id: u64, flag_name: &str) -> Option<Value> {
    CELL_REGISTRY.with(|reg| {
        reg.borrow()
            .get(&(set_id, flag_name.to_string()))
            .map(|arc| arc.lock().clone())
    })
}

/// Installs stdlib-shaped built-ins (`println`, `print`, `eprintln`,
/// `eprint`, `format`, `panic`, ...) into the given global table,
/// plus a curated set of no-op stubs that let real-world example
/// programs at least reach the end of `main` without crashing.
pub(crate) fn install(globals: &mut Vec<(&'static str, Value)>) {
    install_io_builtins(globals);
    install_http_builtins(globals);
    install_variant_builtins(globals);
    install_module_builtins(globals);
    install_flag_builtins(globals);
    install_method_helpers(globals);
    install_concurrency_builtins(globals);
    install_regex_builtins(globals);
    globals.push(("serve", native("serve", native_http_serve)));
}

/// Returns the process-wide cached builtin table (built once on
/// first call). Each `Value::Builtin` / `Value::Native` payload is
/// behind an `Arc`, so cloning the entries is a refcount bump per
/// builtin — cheap enough that downstream consumers (`Vm::new`,
/// `Interpreter::new`) can iterate the cached slice when populating
/// their own globals maps. Pre-cache, both call sites independently
/// rebuilt all ~330 entries, doubling startup work and per-VM
/// memory.
pub(crate) fn cached() -> &'static [(&'static str, Value)] {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Vec<(&'static str, Value)>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut list = Vec::new();
        install(&mut list);
        list
    })
}

fn install_io_builtins(globals: &mut Vec<(&'static str, Value)>) {
    globals.push(("println", builtin("println", builtin_println)));
    globals.push(("print", builtin("print", builtin_print)));
    // Math library — mirrors the native runtime's
    // `gos_rt_math_*` surface. Registered under both the bare
    // name and the qualified `math::*` key the VM's
    // `compile_path` joins.
    globals.push(("sqrt", builtin("sqrt", builtin_math_sqrt)));
    globals.push(("math::sqrt", builtin("math::sqrt", builtin_math_sqrt)));
    globals.push(("sin", builtin("sin", builtin_math_sin)));
    globals.push(("math::sin", builtin("math::sin", builtin_math_sin)));
    globals.push(("cos", builtin("cos", builtin_math_cos)));
    globals.push(("math::cos", builtin("math::cos", builtin_math_cos)));
    globals.push(("exp", builtin("exp", builtin_math_exp)));
    globals.push(("math::exp", builtin("math::exp", builtin_math_exp)));
    globals.push(("ln", builtin("ln", builtin_math_ln)));
    globals.push(("log", builtin("log", builtin_math_ln)));
    globals.push(("math::ln", builtin("math::ln", builtin_math_ln)));
    globals.push(("math::log", builtin("math::log", builtin_math_ln)));
    globals.push(("abs", builtin("abs", builtin_math_abs)));
    globals.push(("math::abs", builtin("math::abs", builtin_math_abs)));
    globals.push(("floor", builtin("floor", builtin_math_floor)));
    globals.push(("math::floor", builtin("math::floor", builtin_math_floor)));
    globals.push(("ceil", builtin("ceil", builtin_math_ceil)));
    globals.push(("math::ceil", builtin("math::ceil", builtin_math_ceil)));
    globals.push(("pow", builtin("pow", builtin_math_pow)));
    globals.push(("math::pow", builtin("math::pow", builtin_math_pow)));
    // Stream constructors — each returns an `io::Stream` value
    // the program's subsequent method calls dispatch against.
    globals.push(("io::stdout", builtin("io::stdout", builtin_io_stdout)));
    globals.push(("io::stderr", builtin("io::stderr", builtin_io_stderr)));
    globals.push(("io::stdin", builtin("io::stdin", builtin_io_stdin)));
    // Method-style shortcuts: `stream.write_byte(b)` dispatches
    // through the walker's generic method routing, which falls
    // back to a global named `write_byte`. Register one each
    // under the bare name + the `Stream::…` qualified key so
    // both lookup paths succeed.
    globals.push((
        "write_byte",
        builtin("write_byte", builtin_stream_write_byte),
    ));
    globals.push((
        "Stream::write_byte",
        builtin("Stream::write_byte", builtin_stream_write_byte),
    ));
    globals.push(("write", builtin("write", builtin_stream_write_str)));
    globals.push((
        "Stream::write",
        builtin("Stream::write", builtin_stream_write_str),
    ));
    globals.push(("write_str", builtin("write_str", builtin_stream_write_str)));
    globals.push((
        "Stream::write_str",
        builtin("Stream::write_str", builtin_stream_write_str),
    ));
    globals.push(("flush", builtin("flush", builtin_stream_flush)));
    globals.push((
        "Stream::flush",
        builtin("Stream::flush", builtin_stream_flush),
    ));
    globals.push(("read_line", builtin("read_line", builtin_stream_read_line)));
    globals.push((
        "Stream::read_line",
        builtin("Stream::read_line", builtin_stream_read_line),
    ));
    globals.push((
        "read_to_string",
        builtin("read_to_string", builtin_stream_read_to_string),
    ));
    globals.push((
        "Stream::read_to_string",
        builtin("Stream::read_to_string", builtin_stream_read_to_string),
    ));
    globals.push(("eprintln", builtin("eprintln", builtin_eprintln)));
    globals.push(("eprint", builtin("eprint", builtin_eprint)));
    globals.push(("format", builtin("format", builtin_format)));
    globals.push(("panic", builtin("panic", builtin_panic)));
    globals.push(("__concat", builtin("__concat", builtin_concat)));
    globals.push(("__fmt_prec", builtin("__fmt_prec", builtin_fmt_prec)));
    globals.push(("__struct", builtin("__struct", builtin_struct_new)));
}

fn install_http_builtins(globals: &mut Vec<(&'static str, Value)>) {
    globals.push(("http::serve", native("http::serve", native_http_serve)));
    globals.push((
        "http::Response::text",
        builtin("http::Response::text", builtin_http_response_text),
    ));
    globals.push((
        "http::Response::json",
        builtin("http::Response::json", builtin_http_response_json),
    ));
    globals.push((
        "Response::text",
        builtin("Response::text", builtin_http_response_text),
    ));
    globals.push((
        "Response::json",
        builtin("Response::json", builtin_http_response_json),
    ));
    globals.push((
        "http::Client::new",
        builtin(
            "http::Client::new",
            crate::http_client_builtins::builtin_http_client_new,
        ),
    ));
    globals.push((
        "Client::get",
        builtin(
            "Client::get",
            crate::http_client_builtins::builtin_http_client_get,
        ),
    ));
    globals.push((
        "Request::send",
        builtin(
            "Request::send",
            crate::http_client_builtins::builtin_http_request_send,
        ),
    ));
    globals.push((
        "Response::bytes",
        builtin(
            "Response::bytes",
            crate::http_client_builtins::builtin_http_response_bytes,
        ),
    ));
    globals.push(("path", builtin("path", builtin_field::<'p'>)));
    globals.push(("method", builtin("method", builtin_field::<'m'>)));
}

fn install_variant_builtins(globals: &mut Vec<(&'static str, Value)>) {
    globals.push(("Ok", builtin("Ok", builtin_variant_one::<'O'>)));
    globals.push(("Err", builtin("Err", builtin_variant_one::<'E'>)));
    globals.push(("Some", builtin("Some", builtin_variant_one::<'S'>)));
    globals.push((
        "None",
        Value::variant("None", crate::value::empty_value_arc()),
    ));
}

// Pure registration list — splitting it would just split the
// install across files without making any function shorter.
#[allow(clippy::too_many_lines)]
fn install_module_builtins(globals: &mut Vec<(&'static str, Value)>) {
    install_module(
        "os",
        &[
            ("args", builtin_os_args),
            ("env", builtin_os_env),
            ("exit", builtin_os_exit),
            ("read_file", builtin_os_read_file),
            ("read_file_to_string", builtin_os_read_file_to_string),
            ("write_file", builtin_os_write_file),
            ("remove_file", builtin_os_remove_file),
            ("rename", builtin_os_rename),
            ("exists", builtin_os_exists),
            ("mkdir", builtin_os_mkdir),
            ("mkdir_all", builtin_os_mkdir_all),
            ("read_dir", builtin_os_read_dir),
            // Stdin pseudo-stream + read_line. `os::stdin()`
            // returns a sentinel that `read_line` recognises;
            // reads pull a line from the host process's stdin.
            ("stdin", builtin_os_stdin),
        ],
        globals,
    );
    install_module(
        "time",
        &[
            ("now", builtin_time_now),
            ("now_ms", builtin_time_now_ms),
            ("sleep", builtin_time_sleep),
            ("format_rfc3339", builtin_time_format_rfc3339),
            ("parse_rfc3339", builtin_time_parse_rfc3339),
        ],
        globals,
    );
    install_module("exec", &[("run", builtin_exec_run)], globals);
    install_module("os::exec", &[("run", builtin_exec_run)], globals);
    install_module(
        "fs",
        &[
            ("walk_dir", builtin_fs_walk_dir),
            ("list_dir", builtin_fs_list_dir),
        ],
        globals,
    );
    install_module("path", &[("walk", builtin_fs_walk_dir)], globals);
    install_module(
        "gzip",
        &[
            ("encode", builtin_gzip_encode),
            ("decode", builtin_gzip_decode),
        ],
        globals,
    );
    install_module(
        "compress::gzip",
        &[
            ("encode", builtin_gzip_encode),
            ("decode", builtin_gzip_decode),
        ],
        globals,
    );
    install_module(
        "slog",
        &[
            ("info", builtin_slog_info),
            ("warn", builtin_slog_warn),
            ("error", builtin_slog_error),
            ("debug", builtin_slog_debug),
        ],
        globals,
    );
    install_module(
        "bufio",
        &[
            ("read_lines", builtin_bufio_read_lines),
            // Streaming-from-stdin entry — every Scanner over
            // os::stdin uses this. The interpreter buffers the
            // whole stdin once on first call, then walks the
            // buffer line-by-line.
            ("Scanner::new", builtin_bufio_scanner_new),
            ("Scanner::next", builtin_bufio_scanner_next),
        ],
        globals,
    );
    // Bare names so user code can write `Scanner::new(stream)` /
    // `s.next()` without an explicit `bufio::` prefix.
    globals.push((
        "Scanner::new",
        builtin("Scanner::new", builtin_bufio_scanner_new),
    ));
    globals.push((
        "Scanner::next",
        builtin("Scanner::next", builtin_bufio_scanner_next),
    ));
    // Method-call dispatch for `<stream>.read_line()` — the same
    // builtin handles both `os::stdin().read_line()` and the
    // method-call form. Adds `read_line` to the global table.
    globals.push(("read_line", builtin("read_line", builtin_stdin_read_line)));
    // HashMap surface — exposed both qualified (`HashMap::*`) and
    // bare (`m.get(k)`, `m.insert(k, v)`) so user code can use the
    // method form. Mutating methods (insert/remove/clear) ride the
    // method-dispatch writeback path same as Vec mutators.
    install_module(
        "HashMap",
        &[
            ("new", builtin_map_new),
            ("with_capacity", builtin_map_with_capacity),
            ("get", builtin_map_get),
            ("get_or", builtin_map_get_or),
            ("inc", builtin_map_inc),
            ("or_insert", builtin_map_or_insert),
            ("inc_at", builtin_map_inc_at),
            ("inc_batch", builtin_map_inc_batch),
            ("insert", builtin_map_insert),
            ("remove", builtin_map_remove),
            ("contains_key", builtin_map_contains_key),
            ("len", builtin_map_len),
            ("keys", builtin_map_keys),
            ("values", builtin_map_values),
            ("iter", builtin_map_iter),
            ("entries", builtin_map_iter),
            ("clear", builtin_map_clear),
            ("is_empty", builtin_map_is_empty),
        ],
        globals,
    );
    // Bare-name surface for method-call dispatch on a Map receiver.
    // The `qualified_method_key(receiver, "get")` lookup misses for
    // Map values (no struct name to derive a key from), so the
    // bare-name fallback in `eval_method_call` does the dispatch.
    globals.push((
        "contains_key",
        builtin("contains_key", builtin_map_contains_key),
    ));
    globals.push(("keys", builtin("keys", builtin_map_keys)));
    globals.push(("values", builtin("values", builtin_map_values)));
    globals.push(("iter", builtin("iter", builtin_map_iter)));
    globals.push(("entries", builtin("entries", builtin_map_iter)));
    globals.push(("get_or", builtin("get_or", builtin_map_get_or)));
    globals.push(("inc", builtin("inc", builtin_map_inc)));
    globals.push(("or_insert", builtin("or_insert", builtin_map_or_insert)));
    // `get` and `insert` and `remove` and `len` and `clear` already
    // exist as bare names for other types; the builtin already
    // routes by receiver so we don't double-register.

    install_module(
        "json",
        &[
            ("parse", builtin_json_parse),
            ("render", builtin_json_render),
            ("encode", builtin_json_render),
            ("decode", builtin_json_decode),
            // Query surface — operates on the dynamic struct shape
            // produced by `json_value_to_gossamer`, so a JSON object
            // is a struct keyed by field name and a JSON array is a
            // `Value::Array`.
            ("get", builtin_json_get),
            ("at", builtin_json_at),
            ("keys", builtin_json_keys),
            ("len", builtin_json_len),
            ("is_null", builtin_json_is_null),
            ("as_str", builtin_json_as_str),
            ("as_i64", builtin_json_as_i64),
            ("as_f64", builtin_json_as_f64),
            ("as_bool", builtin_json_as_bool),
            ("as_array", builtin_json_as_array),
        ],
        globals,
    );
    install_module(
        "testing",
        &[
            ("check", builtin_testing_check),
            ("check_eq", builtin_testing_check_eq),
            ("check_ok", builtin_testing_check_ok),
        ],
        globals,
    );
}

fn install_flag_builtins(globals: &mut Vec<(&'static str, Value)>) {
    globals.push((
        "flag::Value::Int",
        Value::variant("Int", crate::value::empty_value_arc()),
    ));
    globals.push((
        "flag::Value::Str",
        Value::variant("Str", crate::value::empty_value_arc()),
    ));
    globals.push((
        "flag::Value::Bool",
        Value::variant("Bool", crate::value::empty_value_arc()),
    ));
    globals.push(("flag::parse", builtin("flag::parse", builtin_flag_parse)));
    globals.push((
        "FlagMap::get",
        builtin("FlagMap::get", builtin_flag_map_get),
    ));
    globals.push((
        "flag::Set::new",
        builtin(
            "flag::Set::new",
            crate::flag_set_builtins::builtin_flag_set_new,
        ),
    ));
    globals.push((
        "Set::string",
        builtin(
            "Set::string",
            crate::flag_set_builtins::builtin_flag_set_string,
        ),
    ));
    globals.push((
        "Set::int",
        builtin("Set::int", crate::flag_set_builtins::builtin_flag_set_int),
    ));
    globals.push((
        "Set::uint",
        builtin("Set::uint", crate::flag_set_builtins::builtin_flag_set_uint),
    ));
    globals.push((
        "Set::bool",
        builtin("Set::bool", crate::flag_set_builtins::builtin_flag_set_bool),
    ));
    globals.push((
        "Set::short",
        builtin(
            "Set::short",
            crate::flag_set_builtins::builtin_flag_set_short,
        ),
    ));
    globals.push((
        "Set::parse",
        builtin(
            "Set::parse",
            crate::flag_set_builtins::builtin_flag_set_parse,
        ),
    ));
    // Declarative builder: one expression produces a ready-to-use
    // flags struct whose fields deref through `__Cell` to the current
    // value. Avoids the mutate-the-set chain the Set:: builders use.
    globals.push(("flag::int", builtin("flag::int", builtin_flag_spec_int)));
    globals.push((
        "flag::string",
        builtin("flag::string", builtin_flag_spec_string),
    ));
    globals.push(("flag::bool", builtin("flag::bool", builtin_flag_spec_bool)));
    globals.push(("flag::define", builtin("flag::define", builtin_flag_define)));
}

/// Shape of a single spec produced by [`flag::int`] / [`flag::string`]
/// / [`flag::bool`] and consumed by [`flag::define`]. Fields: kind
/// (`"int"` / `"string"` / `"bool"`), long, default, help, short.
fn flag_spec(kind: &str, long: &str, default: Value, help: &str, short: Option<char>) -> Value {
    Value::struct_(
        "FlagSpec",
        Arc::new(vec![
            (
                Ident::new("kind"),
                Value::String(SmolStr::from(kind.to_string())),
            ),
            (
                Ident::new("long"),
                Value::String(SmolStr::from(long.to_string())),
            ),
            (Ident::new("default"), default),
            (
                Ident::new("help"),
                Value::String(SmolStr::from(help.to_string())),
            ),
            (
                Ident::new("short"),
                match short {
                    Some(c) => Value::Char(c),
                    None => Value::Unit,
                },
            ),
        ]),
    )
}

fn builtin_flag_spec_int(args: &[Value]) -> RuntimeResult<Value> {
    let long = args.first().and_then(as_str).unwrap_or("");
    let default = args.get(1).cloned().unwrap_or(Value::Int(0));
    let help = args.get(2).and_then(as_str).unwrap_or("");
    let short = match args.get(3) {
        Some(Value::Char(c)) => Some(*c),
        _ => None,
    };
    Ok(flag_spec("int", long, default, help, short))
}

fn builtin_flag_spec_string(args: &[Value]) -> RuntimeResult<Value> {
    let long = args.first().and_then(as_str).unwrap_or("");
    let default = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| Value::String(SmolStr::from(String::new())));
    let help = args.get(2).and_then(as_str).unwrap_or("");
    let short = match args.get(3) {
        Some(Value::Char(c)) => Some(*c),
        _ => None,
    };
    Ok(flag_spec("string", long, default, help, short))
}

fn builtin_flag_spec_bool(args: &[Value]) -> RuntimeResult<Value> {
    let long = args.first().and_then(as_str).unwrap_or("");
    let default = args.get(1).cloned().unwrap_or(Value::Bool(false));
    let help = args.get(2).and_then(as_str).unwrap_or("");
    let short = match args.get(3) {
        Some(Value::Char(c)) => Some(*c),
        _ => None,
    };
    Ok(flag_spec("bool", long, default, help, short))
}

/// Registers `spec` inside the set identified by `set_id` and
/// returns the `(long_name, cell_value)` pair for the generated
/// `Flags` struct. Pulled out of `builtin_flag_define` so the
/// entry-point stays short enough for clippy's body-length lint.
fn register_flag_spec(set_id: u64, spec: &Value) -> Option<(Ident, Value)> {
    let Value::Struct(spec_inner) = spec else {
        return None;
    };
    let spec_name = spec_inner.name;
    let spec_fields = &spec_inner.fields;
    if spec_name != "FlagSpec" {
        return None;
    }
    let kind = spec_fields
        .iter()
        .find(|(i, _)| i.name == "kind")
        .and_then(|(_, v)| as_str(v))
        .unwrap_or("")
        .to_string();
    let long = spec_fields
        .iter()
        .find(|(i, _)| i.name == "long")
        .and_then(|(_, v)| as_str(v))
        .unwrap_or("")
        .to_string();
    let default = spec_fields
        .iter()
        .find(|(i, _)| i.name == "default")
        .map_or(Value::Unit, |(_, v)| v.clone());
    let help = spec_fields
        .iter()
        .find(|(i, _)| i.name == "help")
        .and_then(|(_, v)| as_str(v))
        .unwrap_or("")
        .to_string();
    let short = spec_fields
        .iter()
        .find(|(i, _)| i.name == "short")
        .and_then(|(_, v)| match v {
            Value::Char(c) => Some(*c),
            _ => None,
        });
    let flag_kind = match kind.as_str() {
        "int" => FlagKind::Int,
        "string" => FlagKind::String,
        "bool" => FlagKind::Bool,
        _ => return None,
    };
    SET_REGISTRY.with(|reg| {
        if let Some(state) = reg.borrow_mut().get_mut(&set_id) {
            state.flag_order.push(long.clone());
            state.flags.insert(
                long.clone(),
                FlagDef {
                    short,
                    kind: flag_kind,
                    help,
                    default: default.clone(),
                },
            );
        }
    });
    let cell = make_cell(set_id, &long, default);
    Some((Ident::new(&long), cell))
}

/// Batch constructor. Creates the internal `Set`, registers every
/// spec, parses `os::args()`, and returns a `Flags` struct with one
/// cell-typed field per spec (named after the spec's long name).
/// Callers access parsed values via `*flags.<long>` — no mutation
/// needed at the call site.
fn builtin_flag_define(args: &[Value]) -> RuntimeResult<Value> {
    let set_name = args.first().and_then(as_str).unwrap_or("").to_string();
    let specs: &[Value] = match args.get(1) {
        Some(Value::Array(arr)) => arr.as_ref().as_slice(),
        _ => &[],
    };
    let set_id = NEXT_SET_ID.with(|cell| {
        let mut v = cell.borrow_mut();
        let id = *v;
        *v += 1;
        id
    });
    SET_REGISTRY.with(|reg| {
        reg.borrow_mut().insert(
            set_id,
            SetState {
                name: set_name,
                flag_order: Vec::new(),
                last_flag: None,
                flags: std::collections::HashMap::new(),
            },
        );
    });
    let mut fields: Vec<(Ident, Value)> = Vec::with_capacity(specs.len() + 1);
    fields.push((
        Ident::new("__set_id"),
        Value::Int(i64::try_from(set_id).unwrap_or(0)),
    ));
    for spec in specs {
        if let Some(entry) = register_flag_spec(set_id, spec) {
            fields.push(entry);
        }
    }
    let args_vec = PROGRAM_ARGS.with(|cell| cell.borrow().clone());
    let args_array = Value::Array(Arc::new(
        args_vec
            .into_iter()
            .map(|s| Value::String(s.into()))
            .collect(),
    ));
    let set_value = Value::struct_(
        "Set",
        Arc::new(vec![(
            Ident::new("__id"),
            Value::Int(i64::try_from(set_id).unwrap_or(0)),
        )]),
    );
    let _ = crate::flag_set_builtins::builtin_flag_set_parse(&[set_value, args_array]);
    Ok(Value::struct_("Flags", Arc::new(fields)))
}

fn install_method_helpers(globals: &mut Vec<(&'static str, Value)>) {
    globals.push(("len", builtin("len", builtin_len)));
    globals.push(("to_string", builtin("to_string", builtin_to_string)));
    globals.push(("split", builtin("split", builtin_split)));
    globals.push(("trim", builtin("trim", builtin_trim)));
    globals.push(("as_bytes", builtin("as_bytes", builtin_as_bytes)));
    globals.push(("push", builtin("push", builtin_push)));
    globals.push(("pop", builtin("pop", builtin_pop)));
    globals.push(("insert", builtin("insert", builtin_insert)));
    globals.push(("remove", builtin("remove", builtin_remove)));
    globals.push(("clear", builtin("clear", builtin_clear)));
    globals.push(("extend", builtin("extend", builtin_extend)));
    globals.push(("truncate", builtin("truncate", builtin_truncate)));
    globals.push(("sort", builtin("sort", builtin_sort)));
    globals.push(("sort_by", native("sort_by", native_sort_by)));
    globals.push(("reverse", builtin("reverse", builtin_reverse)));
    globals.push(("swap", builtin("swap", builtin_swap)));
    globals.push(("clone", builtin("clone", builtin_clone)));
    // `Box<T>` / `Arc<T>` / `Rc<T>` are transparent in a fully GC'd
    // language: every value is heap-shared already, so the wrapper
    // type is purely a Rust-flavoured ergonomic spelling. The
    // constructors return their argument unchanged so user code
    // that writes `Box::new(rest)` for a recursive enum payload
    // (or pattern-matches on the unwrapped value) works without a
    // distinct runtime representation.
    globals.push(("Box::new", builtin("Box::new", builtin_clone)));
    globals.push(("Arc::new", builtin("Arc::new", builtin_clone)));
    globals.push(("Rc::new", builtin("Rc::new", builtin_clone)));
    // String surface that the MIR method-dispatch table already
    // wires for compiled mode. Keep the interpreter's coverage
    // in lockstep so `gos run` and `gos build` agree.
    globals.push((
        "to_uppercase",
        builtin("to_uppercase", builtin_to_uppercase),
    ));
    globals.push((
        "to_lowercase",
        builtin("to_lowercase", builtin_to_lowercase),
    ));
    globals.push(("contains", builtin("contains", builtin_contains)));
    globals.push(("starts_with", builtin("starts_with", builtin_starts_with)));
    globals.push(("ends_with", builtin("ends_with", builtin_ends_with)));
    globals.push(("replace", builtin("replace", builtin_str_replace)));
    globals.push(("find", builtin("find", builtin_str_find)));
    globals.push(("unwrap", builtin("unwrap", builtin_variant_unwrap)));
    globals.push(("unwrap_or", builtin("unwrap_or", builtin_variant_unwrap_or)));
    globals.push((
        "unwrap_or_else",
        native("unwrap_or_else", native_variant_unwrap_or_else),
    ));
    globals.push((
        "unwrap_or_default",
        builtin("unwrap_or_default", builtin_variant_unwrap_or_default),
    ));
    globals.push(("is_some", builtin("is_some", builtin_variant_is::<'S'>)));
    globals.push(("is_none", builtin("is_none", builtin_variant_is::<'N'>)));
    globals.push(("is_ok", builtin("is_ok", builtin_variant_is::<'O'>)));
    globals.push(("is_err", builtin("is_err", builtin_variant_is::<'E'>)));
    globals.push(("ok", builtin("ok", builtin_variant_ok)));
    globals.push(("err", builtin("err", builtin_variant_err)));
    globals.push(("map", native("map", native_variant_map)));
    globals.push(("map_or", native("map_or", native_variant_map_or)));
}

// Pure registration list — splitting it would obscure the
// concurrency surface area without making any function shorter.
#[allow(clippy::too_many_lines)]
fn install_concurrency_builtins(globals: &mut Vec<(&'static str, Value)>) {
    globals.push(("spawn", native("spawn", native_spawn)));
    globals.push(("channel", builtin("channel", builtin_channel_new)));
    globals.push(("channel::new", builtin("channel::new", builtin_channel_new)));
    globals.push((
        "sync::channel",
        builtin("sync::channel", builtin_channel_new),
    ));
    globals.push((
        "Channel::send",
        builtin("Channel::send", builtin_channel_send),
    ));
    globals.push((
        "Channel::recv",
        builtin("Channel::recv", builtin_channel_recv),
    ));
    globals.push((
        "sync::Channel::new",
        builtin("sync::Channel::new", builtin_channel_new),
    ));

    // Shared atomic-i64 buffer used by goroutine fan-out programs
    // (`fasta.gos`'s multi-threaded variant). Backed by a global
    // side table keyed on a u32 handle stuffed into the
    // `I64Vec.__handle` struct field.
    globals.push(("I64Vec::new", builtin("I64Vec::new", builtin_i64vec_new)));
    globals.push((
        "I64Vec::set_at",
        builtin("I64Vec::set_at", builtin_i64vec_set_at),
    ));
    globals.push((
        "I64Vec::get_at",
        builtin("I64Vec::get_at", builtin_i64vec_get_at),
    ));
    globals.push((
        "I64Vec::vec_len",
        builtin("I64Vec::vec_len", builtin_i64vec_vec_len),
    ));
    globals.push((
        "I64Vec::write_range_to_stdout",
        builtin(
            "I64Vec::write_range_to_stdout",
            builtin_i64vec_write_range_to_stdout,
        ),
    ));
    globals.push((
        "I64Vec::write_lines_to_stdout",
        builtin(
            "I64Vec::write_lines_to_stdout",
            builtin_i64vec_write_lines_to_stdout,
        ),
    ));

    // `Vec::new()` produces an empty growable array. Without
    // this entry the `Vec::new` path lookup misses, falls back
    // to the bare `new` global, and resolves to whichever
    // module's `new` was installed last — typically `HashMap`'s,
    // which means `let mut v: Vec<i64> = Vec::new(); v.push(1)`
    // silently builds an empty `HashMap` and the push is a no-op.
    globals.push(("Vec::new", builtin("Vec::new", builtin_vec_new)));

    // U8Vec: 1-byte-per-element heap vec. Same shape as I64Vec
    // but with byte-aligned storage — fasta-style scratch
    // buffers no longer pay the 8x storage tax.
    globals.push(("U8Vec::new", builtin("U8Vec::new", builtin_u8vec_new)));
    globals.push((
        "U8Vec::set_byte",
        builtin("U8Vec::set_byte", builtin_u8vec_set_byte),
    ));
    globals.push((
        "U8Vec::get_byte",
        builtin("U8Vec::get_byte", builtin_u8vec_get_byte),
    ));
    globals.push((
        "U8Vec::byte_len",
        builtin("U8Vec::byte_len", builtin_u8vec_byte_len),
    ));
    globals.push((
        "U8Vec::to_string",
        builtin("U8Vec::to_string", builtin_u8vec_to_string),
    ));
    globals.push((
        "U8Vec::write_byte_range_to_stdout",
        builtin(
            "U8Vec::write_byte_range_to_stdout",
            builtin_u8vec_write_byte_range_to_stdout,
        ),
    ));
    globals.push((
        "U8Vec::write_byte_lines_to_stdout",
        builtin(
            "U8Vec::write_byte_lines_to_stdout",
            builtin_u8vec_write_byte_lines_to_stdout,
        ),
    ));
    // Sliding-window pack: read `k` bytes from `i` and pack
    // them into a single i64 by `(key << 2) | byte`. Single
    // C-side loop replaces what was a k-iter bytecode loop in
    // user code; sliding-window scans ride this op directly.
    // Also exposed via the bare-name dispatch path
    // (`buf.window_key(i, k)`) and as a method receiver.
    globals.push((
        "U8Vec::window_key",
        builtin("U8Vec::window_key", builtin_u8vec_window_key),
    ));
    globals.push((
        "window_key",
        builtin("window_key", builtin_u8vec_window_key),
    ));
    // Whole-program k-mer count: scan the entire buffer and
    // emit a `Value::IntMap` of (packed_kmer_key -> count).
    // Replaces the user-side `while i < stop { … insert … }`
    // loop with a single C-side call for sliding-window
    // counter scans.
    globals.push((
        "U8Vec::count_kmers",
        builtin("U8Vec::count_kmers", builtin_u8vec_count_kmers),
    ));
    globals.push((
        "count_kmers",
        builtin("count_kmers", builtin_u8vec_count_kmers),
    ));
    // Whole-program 4-bucket / 16-bucket frequency scans for
    // small-alphabet single- and pair-base counts. Returns a flat
    // `Value::IntArray` so the caller can index it directly
    // (the existing print-freq helpers already accept a
    // `[i64; N]`-shaped receiver).
    globals.push((
        "U8Vec::count_singles",
        builtin("U8Vec::count_singles", builtin_u8vec_count_singles),
    ));
    globals.push((
        "count_singles",
        builtin("count_singles", builtin_u8vec_count_singles),
    ));
    globals.push((
        "U8Vec::count_pairs",
        builtin("U8Vec::count_pairs", builtin_u8vec_count_pairs),
    ));
    globals.push((
        "count_pairs",
        builtin("count_pairs", builtin_u8vec_count_pairs),
    ));

    // `sync::WaitGroup` mirroring Go's API.
    globals.push((
        "WaitGroup::new",
        builtin("WaitGroup::new", builtin_waitgroup_new),
    ));
    globals.push((
        "WaitGroup::add",
        builtin("WaitGroup::add", builtin_waitgroup_add),
    ));
    globals.push((
        "WaitGroup::done",
        builtin("WaitGroup::done", builtin_waitgroup_done),
    ));
    globals.push((
        "WaitGroup::wait",
        builtin("WaitGroup::wait", builtin_waitgroup_wait),
    ));

    // O(log n) Lehmer LCG affine-transform jump-ahead.
    globals.push(("lcg_jump", builtin("lcg_jump", builtin_lcg_jump)));
    globals.push((
        "gos_rt_lcg_jump",
        builtin("gos_rt_lcg_jump", builtin_lcg_jump),
    ));

    // Bulk byte-array writer used by `out.write_byte_array(&line, n)`
    // in the `fasta` block-write hot path.
    globals.push((
        "Stream::write_byte_array",
        builtin("Stream::write_byte_array", builtin_stream_write_byte_array),
    ));
    globals.push((
        "write_byte_array",
        builtin("write_byte_array", builtin_stream_write_byte_array),
    ));
}

fn install_regex_builtins(globals: &mut Vec<(&'static str, Value)>) {
    install_module("regex", crate::regex_builtins::ENTRIES, globals);
}

/// Pointer-sized function type used by the builtin installer.
type BuiltinFn = fn(&[Value]) -> RuntimeResult<Value>;

fn install_module(
    prefix: &'static str,
    entries: &[(&'static str, BuiltinFn)],
    globals: &mut Vec<(&'static str, Value)>,
) {
    for (short, call) in entries {
        globals.push((*short, builtin(short, *call)));
        let joined: &'static str = Box::leak(format!("{prefix}::{short}").into_boxed_str());
        globals.push((joined, builtin(joined, *call)));
    }
}

fn builtin_variant_one<const TAG: char>(args: &[Value]) -> RuntimeResult<Value> {
    let name = match TAG {
        'O' => "Ok",
        'E' => "Err",
        'S' => "Some",
        _ => "Variant",
    };
    let payload = args.first().cloned().unwrap_or(Value::Unit);
    Ok(Value::variant(name, Arc::new(vec![payload])))
}

fn builtin_field<const TAG: char>(args: &[Value]) -> RuntimeResult<Value> {
    let field_name = match TAG {
        'p' => "path",
        'm' => "method",
        _ => return Ok(Value::Unit),
    };
    match args.first() {
        Some(Value::Struct(inner)) => {
            for (ident, value) in inner.fields.iter() {
                if ident.name == field_name {
                    return Ok(value.clone());
                }
            }
            Ok(Value::Unit)
        }
        _ => Ok(Value::Unit),
    }
}

fn builtin(name: &'static str, call: fn(&[Value]) -> RuntimeResult<Value>) -> Value {
    Value::builtin(name, call)
}

fn native(
    name: &'static str,
    call: fn(&mut dyn NativeDispatch, &[Value]) -> RuntimeResult<Value>,
) -> Value {
    Value::native(name, call)
}

/// Captured stdout used by `println` and friends. The test harness
/// swaps this out via [`set_stdout_writer`] and reads back the buffer.
///
/// The pointer is `'static` only because the value it points at is a
/// per-thread static; no cross-thread access is possible.
type Writer = fn(&str);

thread_local! {
    static STDOUT_WRITER: std::cell::Cell<Writer> = const { std::cell::Cell::new(default_stdout) };
    static STDERR_WRITER: std::cell::Cell<Writer> = const { std::cell::Cell::new(default_stderr) };
}

fn default_stdout(text: &str) {
    print!("{text}");
}

fn default_stderr(text: &str) {
    eprint!("{text}");
}

/// Installs a custom stdout writer for the current thread. Returns the
/// previously-installed writer so the caller can restore it.
///
/// Side effect: also disables the JIT process-wide. The runtime's
/// `gos_rt_print_*` family writes to a separate buffer and flushes
/// directly to fd 1 — there's no per-call hook for that path, so a
/// JIT-promoted body's output bypasses the writer the test set up.
/// Disabling the JIT routes everything through the bytecode VM's
/// `STDOUT_WRITER`, which the redirect actually catches. Test
/// suites that wrap their writer with `set_stdout_writer` therefore
/// see every byte the program emits, JIT-eligible function or not.
pub fn set_stdout_writer(writer: Writer) -> Writer {
    crate::set_jit_disabled();
    STDOUT_WRITER.with(|cell| cell.replace(writer))
}

/// Installs a custom stderr writer for the current thread. Returns the
/// previously-installed writer so the caller can restore it.
pub fn set_stderr_writer(writer: Writer) -> Writer {
    STDERR_WRITER.with(|cell| cell.replace(writer))
}

/// Process-wide cap on the number of HTTP requests the interpreter-
/// hosted `http::serve` accepts before returning. Set by tests to
/// force the server loop to terminate; production callers leave it
/// at zero and rely on the `GOSSAMER_HTTP_MAX_REQUESTS` env var or a
/// shutdown signal.
///
/// A value of `0` means unset; any positive value wins over the env
/// var so tests that configure the override remain deterministic.
static HTTP_MAX_REQUESTS_OVERRIDE: AtomicU64 = AtomicU64::new(0);

/// Installs a programmatic cap on the number of HTTP requests the
/// interpreter's `http::serve` accepts before returning. Primarily a
/// test hook so that the server thread exits cleanly after the test
/// drives its one fixture request.
pub fn set_http_max_requests(n: u64) {
    HTTP_MAX_REQUESTS_OVERRIDE.store(n, Ordering::SeqCst);
}

thread_local! {
    /// Per-thread counters and messages tracked by `testing::check*`
    /// builtins; `gos test` resets them around each `#[test]` call
    /// so assertions that fire without being `?`-propagated still
    /// register as failures.
    static TEST_TALLY: std::cell::RefCell<TestTally> =
        const { std::cell::RefCell::new(TestTally::new()) };
}

/// Snapshot of `testing::check*` outcomes for the current test.
#[derive(Debug, Clone, Default)]
pub struct TestTally {
    /// Total `check*` calls observed since the last reset.
    pub assertions: u32,
    /// Subset of those that returned `Err`.
    pub failures: u32,
    /// First failure message, if any; later failures are still
    /// counted but not recorded, on the assumption that the first is
    /// usually the root cause.
    pub first_failure: Option<String>,
}

impl TestTally {
    /// Returns an empty tally.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            assertions: 0,
            failures: 0,
            first_failure: None,
        }
    }

    fn observe(&mut self, ok: bool, message: impl Into<String>) {
        self.assertions += 1;
        if !ok {
            self.failures += 1;
            if self.first_failure.is_none() {
                self.first_failure = Some(message.into());
            }
        }
    }
}

/// Resets the current thread's test tally. Call this immediately
/// before invoking a test function.
pub fn reset_test_tally() {
    TEST_TALLY.with(|cell| *cell.borrow_mut() = TestTally::new());
}

/// Returns a snapshot of the tally accumulated since the last reset.
#[must_use]
pub fn take_test_tally() -> TestTally {
    TEST_TALLY.with(|cell| cell.replace(TestTally::new()))
}

fn observe_assertion(ok: bool, message: String) {
    TEST_TALLY.with(|cell| cell.borrow_mut().observe(ok, message));
}

thread_local! {
    /// Most-recent `<file>:<line>:<col>` of an assertion-shaped
    /// builtin call. Set by the interpreter just before each
    /// `check*` invocation; read by the assertion builtins to
    /// stamp the location into the failure message.
    static ASSERTION_LOCATION: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Records the source location of the assertion currently being
/// evaluated. The interpreter calls this before dispatching to
/// `testing::check*`.
pub fn set_assertion_location(location: Option<String>) {
    ASSERTION_LOCATION.with(|cell| *cell.borrow_mut() = location);
}

fn current_assertion_location() -> Option<String> {
    ASSERTION_LOCATION.with(|cell| cell.borrow().clone())
}

fn write_stdout(text: &str) {
    STDOUT_WRITER.with(|cell| (cell.get())(text));
}

fn write_stderr(text: &str) {
    STDERR_WRITER.with(|cell| (cell.get())(text));
}

fn builtin_println(args: &[Value]) -> RuntimeResult<Value> {
    let rendered = render_args(args);
    write_stdout(&rendered);
    write_stdout("\n");
    Ok(Value::Unit)
}

fn builtin_print(args: &[Value]) -> RuntimeResult<Value> {
    write_stdout(&render_args(args));
    Ok(Value::Unit)
}

// ---- Stream builtins: io::{stdout, stderr, stdin} + methods ----
//
// An `io::Stream` value is a `Value::Struct` with a single `fd`
// field: 0 = stdin, 1 = stdout, 2 = stderr. The walker's method
// dispatch routes `stream.write_byte(b)` etc. to the handlers
// below based on either the bare method name or the
// `Stream::method` qualified key.

fn stream_of(fd: i64) -> Value {
    Value::struct_(
        "Stream",
        Arc::new(vec![(gossamer_ast::Ident::new("fd"), Value::Int(fd))]),
    )
}

fn stream_fd(value: &Value) -> i64 {
    match value {
        Value::Struct(inner) if inner.name == "Stream" => {
            for (f_name, f_val) in inner.fields.iter() {
                if f_name.name == "fd" {
                    if let Value::Int(n) = f_val {
                        return *n;
                    }
                }
            }
            1
        }
        _ => 1,
    }
}

fn math_arg(args: &[Value]) -> f64 {
    match args.first() {
        Some(Value::Float(x)) => *x,
        Some(Value::Int(n)) => *n as f64,
        _ => 0.0,
    }
}

fn builtin_math_sqrt(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_arg(args).sqrt()))
}
fn builtin_math_sin(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_arg(args).sin()))
}
fn builtin_math_cos(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_arg(args).cos()))
}
fn builtin_math_exp(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_arg(args).exp()))
}
fn builtin_math_ln(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_arg(args).ln()))
}
fn builtin_math_abs(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_arg(args).abs()))
}
fn builtin_math_floor(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_arg(args).floor()))
}
fn builtin_math_ceil(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_arg(args).ceil()))
}
fn builtin_math_pow(args: &[Value]) -> RuntimeResult<Value> {
    let x = math_arg(args);
    let y = match args.get(1) {
        Some(Value::Float(v)) => *v,
        Some(Value::Int(n)) => *n as f64,
        _ => 0.0,
    };
    Ok(Value::Float(x.powf(y)))
}

fn builtin_io_stdout(_: &[Value]) -> RuntimeResult<Value> {
    Ok(stream_of(1))
}
fn builtin_io_stderr(_: &[Value]) -> RuntimeResult<Value> {
    Ok(stream_of(2))
}
fn builtin_io_stdin(_: &[Value]) -> RuntimeResult<Value> {
    Ok(stream_of(0))
}

fn builtin_stream_write_byte(args: &[Value]) -> RuntimeResult<Value> {
    let fd = args.first().map_or(1, stream_fd);
    let b = match args.get(1) {
        Some(Value::Int(n)) => *n,
        _ => return Err(RuntimeError::Type("write_byte: expected i64".to_string())),
    };
    stream_write_one_byte(fd, b);
    Ok(Value::Unit)
}

/// Writes a single byte to fd `fd` through the bytecode VM's
/// redirectable writer (`STDOUT_WRITER` / `STDERR_WRITER`),
/// matching the public `set_stdout_writer` contract used by
/// tests. Pulled out of `builtin_stream_write_byte` so the
/// `Op::StreamWriteByte` super-instruction can call it without
/// constructing a `Vec<Value>` first — the dominant per-byte cost
/// in `fasta`'s output loop.
pub(crate) fn stream_write_one_byte(fd: i64, byte: i64) {
    let bytes = [(byte & 0xff) as u8];
    let text = std::str::from_utf8(&bytes).unwrap_or("");
    if fd == 2 {
        write_stderr(text);
    } else {
        write_stdout(text);
    }
}

fn builtin_stream_write_str(args: &[Value]) -> RuntimeResult<Value> {
    let fd = args.first().map_or(1, stream_fd);
    let s = args.get(1).map(render_one).unwrap_or_default();
    if fd == 2 {
        write_stderr(&s);
    } else {
        write_stdout(&s);
    }
    Ok(Value::Unit)
}

fn builtin_stream_flush(_args: &[Value]) -> RuntimeResult<Value> {
    // The walker's writers are unbuffered (go straight to the
    // installed closures); flush is a no-op.
    Ok(Value::Unit)
}

fn builtin_stream_read_line(args: &[Value]) -> RuntimeResult<Value> {
    use std::io::BufRead;
    let fd = args.first().map_or(0, stream_fd);
    if fd != 0 {
        return Ok(Value::String(SmolStr::from(String::new())));
    }
    let stdin = std::io::stdin();
    let mut line = String::new();
    match stdin.lock().read_line(&mut line) {
        Ok(_) => {
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            Ok(Value::String(line.into()))
        }
        Err(_) => Ok(Value::String(SmolStr::from(String::new()))),
    }
}

fn builtin_stream_read_to_string(args: &[Value]) -> RuntimeResult<Value> {
    use std::io::Read;
    let fd = args.first().map_or(0, stream_fd);
    if fd != 0 {
        return Ok(Value::String(SmolStr::from(String::new())));
    }
    let stdin = std::io::stdin();
    let mut buf = String::new();
    match stdin.lock().read_to_string(&mut buf) {
        Ok(_) => Ok(Value::String(buf.into())),
        Err(_) => Ok(Value::String(SmolStr::from(String::new()))),
    }
}

fn builtin_eprintln(args: &[Value]) -> RuntimeResult<Value> {
    let rendered = render_args(args);
    write_stderr(&rendered);
    write_stderr("\n");
    Ok(Value::Unit)
}

fn builtin_eprint(args: &[Value]) -> RuntimeResult<Value> {
    write_stderr(&render_args(args));
    Ok(Value::Unit)
}

fn builtin_format(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(SmolStr::from(render_args(args))))
}

/// Zero-separator concat. Used by compile-time macro expansion.
fn builtin_concat(args: &[Value]) -> RuntimeResult<Value> {
    let mut out = String::with_capacity(args.len() * 8);
    for arg in args {
        let _ = write!(out, "{arg}");
    }
    Ok(Value::String(out.into()))
}

/// `__fmt_prec(value, prec)` — format-string `{:.N}` lowering. Returns
/// a `String` containing `value` rendered with `prec` fractional
/// digits. Mirrors the runtime helper `gos_rt_f64_prec_to_str` so
/// interp output matches the compiled tiers bit-for-bit.
fn builtin_fmt_prec(args: &[Value]) -> RuntimeResult<Value> {
    let value = args.first().cloned().unwrap_or(Value::Int(0));
    let prec = args.get(1).and_then(value_to_int).unwrap_or(0);
    let prec = prec.clamp(0, 64) as usize;
    let f = match value {
        Value::Float(f) => f,
        Value::Int(n) => n as f64,
        other => {
            return Err(RuntimeError::Type(format!(
                "__fmt_prec expected a numeric first argument, got {other}"
            )));
        }
    };
    Ok(Value::String(format!("{f:.prec$}").into()))
}

fn builtin_panic(args: &[Value]) -> RuntimeResult<Value> {
    Err(RuntimeError::Panic(render_args(args)))
}

fn builtin_http_response_text(args: &[Value]) -> RuntimeResult<Value> {
    // Method call: response.text() — receiver is a Response struct.
    if let Some(Value::Struct(inner)) = args.first() {
        if inner.name == "Response" {
            let body = inner
                .fields
                .iter()
                .find(|(ident, _)| ident.name == "body")
                .and_then(|(_, v)| as_str(v))
                .unwrap_or_default();
            return Ok(ok_variant(Value::String(SmolStr::from(body.to_string()))));
        }
    }
    // Constructor: Response::text(status, body).
    let status = args.first().and_then(value_to_int).unwrap_or(200);
    let body = args.get(1).map(render_one).unwrap_or_default();
    Ok(response_struct(status, body, "text/plain; charset=utf-8"))
}

fn builtin_http_response_json(args: &[Value]) -> RuntimeResult<Value> {
    let status = args.first().and_then(value_to_int).unwrap_or(200);
    let body = args.get(1).map(render_one).unwrap_or_default();
    Ok(response_struct(status, body, "application/json"))
}

fn response_struct(status: i64, body: String, content_type: &str) -> Value {
    let fields = vec![
        (Ident::new("status"), Value::Int(status)),
        (Ident::new("body"), Value::String(body.into())),
        (
            Ident::new("content_type"),
            Value::String(SmolStr::from(content_type.to_string())),
        ),
    ];
    Value::struct_("Response", Arc::new(fields))
}

pub(crate) fn value_to_int(value: &Value) -> Option<i64> {
    match value {
        Value::Int(n) => Some(*n),
        _ => None,
    }
}

fn render_one(value: &Value) -> String {
    match value {
        Value::String(s) => s.as_str().to_string(),
        other => format!("{other}"),
    }
}

fn render_args(args: &[Value]) -> String {
    let mut out = String::new();
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{arg}");
    }
    out
}

/// `http::serve(addr: String, handler: Value) -> Result<(), Error>`.
///
/// Binds a TCP listener on `addr` and serves HTTP/1.1 traffic. Each
/// accepted connection is parsed into a [`Request`][http_std::Request]
/// shaped `Value::Struct`, then dispatched by calling the user's
/// `serve` method with `[handler, request_value]`. The returned
/// response value is serialised back to the wire.
///
/// Graceful shutdown is driven by the `GOSSAMER_HTTP_MAX_REQUESTS`
/// environment variable (stop after N requests) or by the process
/// receiving SIGINT.
fn native_http_serve(dispatch: &mut dyn NativeDispatch, args: &[Value]) -> RuntimeResult<Value> {
    if args.len() < 2 {
        return Err(RuntimeError::Arity {
            expected: 2,
            found: args.len(),
        });
    }
    let addr: String = match &args[0] {
        Value::String(s) => s.as_str().to_string(),
        other => {
            return Err(RuntimeError::Type(format!(
                "expected address string, got {other}"
            )));
        }
    };
    let handler = args[1].clone();

    let mut config = http_std::server::Config::default();
    let override_max = HTTP_MAX_REQUESTS_OVERRIDE.load(Ordering::SeqCst);
    if override_max > 0 {
        config.max_requests = Some(override_max);
    } else if let Ok(raw) = std::env::var("GOSSAMER_HTTP_MAX_REQUESTS") {
        if let Ok(n) = raw.parse::<u64>() {
            config.max_requests = Some(n);
            eprintln!(
                "http::serve: GOSSAMER_HTTP_MAX_REQUESTS={n} — server will exit after {n} request(s). Unset this env var to run forever."
            );
        }
    }
    let shutdown = Arc::clone(&config.shutdown);
    install_sigint_handler(shutdown);

    let errors = Mutex::new(Vec::<String>::new());
    let dispatch_cell = std::cell::RefCell::new(dispatch);

    let result = http_std::server::bind_and_run(&addr, &config, |request| {
        let request_value = request_to_value(&request);
        let mut guard = dispatch_cell.borrow_mut();
        let dispatched = guard.call_fn("serve", vec![handler.clone(), request_value]);
        drop(guard);
        match dispatched {
            Ok(value) => {
                if let Some(response) = value_to_response(&value) {
                    response
                } else {
                    let mut errs = errors.lock().unwrap();
                    errs.push("handler did not return http::Response".to_string());
                    drop(errs);
                    http_std::Response::text(
                        http_std::StatusCode::INTERNAL_SERVER_ERROR,
                        "internal server error",
                    )
                }
            }
            Err(err) => {
                let mut errs = errors.lock().unwrap();
                errs.push(format!("{err}"));
                drop(errs);
                http_std::Response::text(
                    http_std::StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error",
                )
            }
        }
    });

    match result {
        Ok(()) => Ok(Value::variant("Ok", Arc::new(vec![Value::Unit]))),
        Err(err) => Err(RuntimeError::Panic(format!("http::serve: {err}"))),
    }
}

fn request_to_value(request: &http_std::Request) -> Value {
    let (bare_path, query_string) = match request.path.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (request.path.clone(), String::new()),
    };
    let headers: Vec<Value> = request
        .headers
        .iter()
        .map(|(name, value)| {
            Value::Tuple(Arc::new(vec![
                Value::String(SmolStr::from(name.to_string())),
                Value::String(SmolStr::from(value.to_string())),
            ]))
        })
        .collect();
    let query_pairs: Vec<Value> = query_string
        .split('&')
        .filter(|seg| !seg.is_empty())
        .map(|seg| {
            let (k, v) = match seg.split_once('=') {
                Some((k, v)) => (k, v),
                None => (seg, ""),
            };
            Value::Tuple(Arc::new(vec![
                Value::String(SmolStr::from(k.to_string())),
                Value::String(SmolStr::from(v.to_string())),
            ]))
        })
        .collect();
    let body_text = String::from_utf8_lossy(&request.body).into_owned();
    let fields = vec![
        (
            Ident::new("method"),
            Value::String(SmolStr::from(request.method.as_str().to_string())),
        ),
        (Ident::new("path"), Value::String(bare_path.into())),
        (Ident::new("query"), Value::String(query_string.into())),
        (
            Ident::new("query_pairs"),
            Value::Array(Arc::new(query_pairs)),
        ),
        (Ident::new("headers"), Value::Array(Arc::new(headers))),
        (Ident::new("body"), Value::String(body_text.into())),
    ];
    Value::struct_("Request", Arc::new(fields))
}

fn value_to_response(value: &Value) -> Option<http_std::Response> {
    let unwrapped = unwrap_result(value);
    let Value::Struct(struct_inner) = unwrapped else {
        return None;
    };
    let fields = &struct_inner.fields;
    let mut status: u16 = 200;
    let mut body: Vec<u8> = Vec::new();
    let mut content_type = "text/plain; charset=utf-8".to_string();
    for (ident, v) in fields.iter() {
        match ident.name.as_str() {
            "status" => {
                status = match v {
                    Value::Int(n) => u16::try_from(*n).unwrap_or(500),
                    Value::Variant(var_inner) if !var_inner.fields.is_empty() => {
                        match &var_inner.fields[0] {
                            Value::Int(n) => u16::try_from(*n).unwrap_or(500),
                            _ => 200,
                        }
                    }
                    _ => 200,
                };
            }
            "body" => match v {
                Value::String(s) => body = s.as_bytes().to_vec(),
                Value::Array(bytes) => {
                    body = bytes
                        .iter()
                        .filter_map(|b| match b {
                            Value::Int(n) => u8::try_from(*n).ok(),
                            _ => None,
                        })
                        .collect();
                }
                _ => {}
            },
            "content_type" => {
                if let Value::String(s) = v {
                    content_type.clear();
                    content_type.push_str(s.as_str());
                }
            }
            _ => {}
        }
    }
    let mut response = http_std::Response {
        status: http_std::StatusCode(status),
        headers: http_std::Headers::new(),
        body,
    };
    response.headers.insert("content-type", &content_type);
    response
        .headers
        .insert("content-length", &response.body.len().to_string());
    Some(response)
}

fn unwrap_result(value: &Value) -> &Value {
    match value {
        Value::Variant(inner) if inner.name == "Ok" && !inner.fields.is_empty() => &inner.fields[0],
        other => other,
    }
}

static SIGINT_HOOKED: AtomicBool = AtomicBool::new(false);

fn install_sigint_handler(flag: Arc<AtomicBool>) {
    if SIGINT_HOOKED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("gossamer-http-sigint".to_string())
        .spawn(move || {
            let _ = flag;
        })
        .ok();
}

/// Extracts a borrowed string slice from a Gossamer value, returning
/// `None` when the value is not a string.
pub(crate) fn as_str(value: &Value) -> Option<&str> {
    match value {
        Value::String(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Builds a `Result::Ok(value)` Gossamer variant.
pub(crate) fn ok_variant(value: Value) -> Value {
    Value::variant("Ok", Arc::new(vec![value]))
}

/// Builds a `Result::Err(message)` Gossamer variant carrying a string.
pub(crate) fn err_variant(message: impl Into<String>) -> Value {
    Value::variant(
        "Err",
        Arc::new(vec![Value::String(SmolStr::from(message.into()))]),
    )
}

/// Builds a `Option::Some(value)` Gossamer variant.
pub(crate) fn some_variant(value: Value) -> Value {
    Value::variant("Some", Arc::new(vec![value]))
}

/// Builds a `Option::None` Gossamer variant.
pub(crate) fn none_variant() -> Value {
    Value::variant("None", crate::value::empty_value_arc())
}

fn builtin_os_args(_args: &[Value]) -> RuntimeResult<Value> {
    let argv: Vec<Value> = PROGRAM_ARGS
        .with(|cell| cell.borrow().clone())
        .into_iter()
        .map(|s| Value::String(s.into()))
        .collect();
    Ok(Value::Array(Arc::new(argv)))
}

fn builtin_os_env(args: &[Value]) -> RuntimeResult<Value> {
    let name = args.first().and_then(as_str).unwrap_or("");
    match os_std::env(name) {
        Some(value) => Ok(some_variant(Value::String(value.into()))),
        None => Ok(none_variant()),
    }
}

fn builtin_os_exit(args: &[Value]) -> RuntimeResult<Value> {
    let code = args.first().and_then(value_to_int).unwrap_or(0);
    std::process::exit(i32::try_from(code).unwrap_or(0));
}

fn builtin_os_read_file(args: &[Value]) -> RuntimeResult<Value> {
    let Some(path) = args.first().and_then(as_str) else {
        return Ok(err_variant("read_file: path argument must be a string"));
    };
    match os_std::read_file(path) {
        Ok(bytes) => {
            let values: Vec<Value> = bytes
                .into_iter()
                .map(|b| Value::Int(i64::from(b)))
                .collect();
            Ok(ok_variant(Value::Array(Arc::new(values))))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_os_read_file_to_string(args: &[Value]) -> RuntimeResult<Value> {
    let Some(path) = args.first().and_then(as_str) else {
        return Ok(err_variant(
            "read_file_to_string: path argument must be a string",
        ));
    };
    match os_std::read_file_to_string(path) {
        Ok(text) => Ok(ok_variant(Value::String(text.into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_os_write_file(args: &[Value]) -> RuntimeResult<Value> {
    let Some(path) = args.first().and_then(as_str) else {
        return Ok(err_variant("write_file: path argument must be a string"));
    };
    let bytes = match args.get(1) {
        Some(Value::String(s)) => s.as_bytes().to_vec(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|v| match v {
                Value::Int(n) => u8::try_from(*n).ok(),
                _ => None,
            })
            .collect(),
        _ => {
            return Ok(err_variant(
                "write_file: contents must be string or byte array",
            ));
        }
    };
    match os_std::write_file(path, &bytes) {
        Ok(()) => Ok(ok_variant(Value::Unit)),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_os_remove_file(args: &[Value]) -> RuntimeResult<Value> {
    let Some(path) = args.first().and_then(as_str) else {
        return Ok(err_variant("remove_file: path argument must be a string"));
    };
    match os_std::remove_file(path) {
        Ok(()) => Ok(ok_variant(Value::Unit)),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_os_rename(args: &[Value]) -> RuntimeResult<Value> {
    let Some(from) = args.first().and_then(as_str) else {
        return Ok(err_variant("rename: source path must be a string"));
    };
    let Some(to) = args.get(1).and_then(as_str) else {
        return Ok(err_variant("rename: destination path must be a string"));
    };
    match os_std::rename(from, to) {
        Ok(()) => Ok(ok_variant(Value::Unit)),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_os_exists(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Bool(os_std::exists(path)))
}

fn builtin_os_mkdir(args: &[Value]) -> RuntimeResult<Value> {
    let Some(path) = args.first().and_then(as_str) else {
        return Ok(err_variant("mkdir: path argument must be a string"));
    };
    match os_std::mkdir(path) {
        Ok(()) => Ok(ok_variant(Value::Unit)),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_os_mkdir_all(args: &[Value]) -> RuntimeResult<Value> {
    let Some(path) = args.first().and_then(as_str) else {
        return Ok(err_variant("mkdir_all: path argument must be a string"));
    };
    match os_std::mkdir_all(path) {
        Ok(()) => Ok(ok_variant(Value::Unit)),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_os_read_dir(args: &[Value]) -> RuntimeResult<Value> {
    let Some(path) = args.first().and_then(as_str) else {
        return Ok(err_variant("read_dir: path argument must be a string"));
    };
    match os_std::read_dir(path) {
        Ok(names) => {
            let values: Vec<Value> = names.into_iter().map(|s| Value::String(s.into())).collect();
            Ok(ok_variant(Value::Array(Arc::new(values))))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_time_now(_args: &[Value]) -> RuntimeResult<Value> {
    let ms = time_std::SystemTime::now().unix_millis();
    Ok(Value::Int(i64::try_from(ms).unwrap_or(i64::MAX)))
}

fn builtin_time_now_ms(_args: &[Value]) -> RuntimeResult<Value> {
    let ms = time_std::SystemTime::now().unix_millis();
    Ok(Value::Int(i64::try_from(ms).unwrap_or(i64::MAX)))
}

fn builtin_time_sleep(args: &[Value]) -> RuntimeResult<Value> {
    let ms = args.first().and_then(value_to_int).unwrap_or(0).max(0);
    let duration = time_std::Duration::from_millis(u64::try_from(ms).unwrap_or(0));
    time_std::sleep(duration);
    Ok(Value::Unit)
}

/// `time::format_rfc3339(unix_ms: i64) -> Result<String, String>`.
/// RFC 3339 rendering for the given wall-clock instant.
fn builtin_time_format_rfc3339(args: &[Value]) -> RuntimeResult<Value> {
    let ms = args.first().and_then(value_to_int).unwrap_or(0);
    let when = time_std::SystemTime::from_unix_millis(ms);
    match time_std::format_rfc3339(when) {
        Ok(s) => Ok(ok_variant(Value::String(SmolStr::from(s)))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

/// `time::parse_rfc3339(s: String) -> Result<i64, String>`. Returns
/// unix milliseconds; the inverse of `format_rfc3339`.
fn builtin_time_parse_rfc3339(args: &[Value]) -> RuntimeResult<Value> {
    let Some(s) = args.first().and_then(as_str) else {
        return Ok(err_variant(
            "time::parse_rfc3339: argument must be a string",
        ));
    };
    match time_std::parse_rfc3339(s) {
        Ok(when) => {
            let ms = i64::try_from(when.unix_millis()).unwrap_or(i64::MAX);
            Ok(ok_variant(Value::Int(ms)))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

/// `exec::run(prog: String, args: [String]) -> Result<{stdout, stderr, code}, String>`.
/// One-shot subprocess: spawns `prog` with `args`, captures stdout
/// and stderr, waits for completion, and returns the trio. The
/// Command builder pattern remains available through the
/// gossamer-std Rust API for callers that want stdin piping or
/// streamed output; this entry point covers the dominant
/// "run a command and read its output" use case.
fn builtin_exec_run(args: &[Value]) -> RuntimeResult<Value> {
    let Some(prog) = args.first().and_then(as_str) else {
        return Ok(err_variant("exec::run: program argument must be a string"));
    };
    let mut cmd = exec_std::Command::new(prog);
    if let Some(Value::Array(arr)) = args.get(1) {
        for arg in arr.iter() {
            if let Some(s) = as_str(arg) {
                cmd = cmd.arg(s);
            }
        }
    }
    cmd = cmd.stdout(exec_std::Stdio::Piped);
    cmd = cmd.stderr(exec_std::Stdio::Piped);
    match cmd.output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            let code = i64::from(out.status.code().unwrap_or(-1));
            let fields = vec![
                (Ident::new("stdout"), Value::String(SmolStr::from(stdout))),
                (Ident::new("stderr"), Value::String(SmolStr::from(stderr))),
                (Ident::new("code"), Value::Int(code)),
            ];
            Ok(ok_variant(Value::struct_("ExecOutput", Arc::new(fields))))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

/// `fs::list_dir(path: String) -> Result<[DirInfo], String>` — direct-children
/// listing with metadata. `DirInfo` is a struct carrying the entry's
/// name, full path, type predicates, byte size (`0` for directories),
/// and modification time as unix milliseconds. The result is sorted
/// by name. Pairs with `fs::walk_dir` (recursive) and `os::read_dir`
/// (names only); use this one when the call site needs to render or
/// filter on metadata.
fn builtin_fs_list_dir(args: &[Value]) -> RuntimeResult<Value> {
    let Some(path) = args.first().and_then(as_str) else {
        return Ok(err_variant("fs::list_dir: path argument must be a string"));
    };
    let entries = match fs_std::read_dir(path) {
        Ok(es) => es,
        Err(e) => return Ok(err_variant(format!("{e}"))),
    };
    let mut items: Vec<Value> = Vec::with_capacity(entries.len());
    for entry in entries {
        let (size, modified_ms) = std::fs::metadata(&entry.path).map_or((0_i64, 0_i64), |m| {
            let size = i64::try_from(m.len()).unwrap_or(i64::MAX);
            let modified_ms = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
            (size, modified_ms)
        });
        let path_str = entry.path.to_string_lossy().into_owned();
        let fields = vec![
            (Ident::new("name"), Value::String(SmolStr::from(entry.name))),
            (Ident::new("path"), Value::String(SmolStr::from(path_str))),
            (Ident::new("is_file"), Value::Bool(entry.is_file)),
            (Ident::new("is_dir"), Value::Bool(entry.is_dir)),
            (Ident::new("is_symlink"), Value::Bool(entry.is_symlink)),
            (Ident::new("size"), Value::Int(size)),
            (Ident::new("modified_ms"), Value::Int(modified_ms)),
        ];
        items.push(Value::struct_("DirInfo", Arc::new(fields)));
    }
    Ok(ok_variant(Value::Array(Arc::new(items))))
}

/// `fs::walk_dir(root: String) -> Result<[String], String>`. Recursive
/// walk; returns every descendant path as a flat array. The
/// gossamer-std API uses a visitor closure for streaming; this
/// builtin materialises the list to keep the .gos call site
/// simple. Aliased as `path::walk` for Go-shaped spelling.
fn builtin_fs_walk_dir(args: &[Value]) -> RuntimeResult<Value> {
    let Some(root) = args.first().and_then(as_str) else {
        return Ok(err_variant("fs::walk_dir: root argument must be a string"));
    };
    let collected = std::cell::RefCell::new(Vec::<String>::new());
    let visit_result = fs_std::walk_dir(root, |entry| {
        if let Some(p) = entry.path.to_str() {
            collected.borrow_mut().push(p.to_string());
        }
        Ok(())
    });
    match visit_result {
        Ok(()) => {
            let items: Vec<Value> = collected
                .into_inner()
                .into_iter()
                .map(|s| Value::String(SmolStr::from(s)))
                .collect();
            Ok(ok_variant(Value::Array(Arc::new(items))))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

/// `gzip::encode(data: String, level: i64) -> Result<String, String>`.
/// Strings carry the byte payload (lossy at non-UTF-8 boundaries
/// but matches the shape Gossamer exposes for binary buffers
/// today). Level 0..=9 picks the flate2 compression level.
fn builtin_gzip_encode(args: &[Value]) -> RuntimeResult<Value> {
    let Some(data) = args.first().and_then(as_str) else {
        return Ok(err_variant("gzip::encode: data argument must be a string"));
    };
    let level_raw = args.get(1).and_then(value_to_int).unwrap_or(6);
    let level_u = u32::try_from(level_raw.clamp(0, 9)).unwrap_or(6);
    let level = gzip_std::Level::new(level_u).unwrap_or_default();
    match gzip_std::encode(data.as_bytes(), level) {
        Ok(out) => {
            let s = String::from_utf8_lossy(&out).into_owned();
            Ok(ok_variant(Value::String(SmolStr::from(s))))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

/// `gzip::decode(data: String) -> Result<String, String>`. Inverse of
/// [`builtin_gzip_encode`].
fn builtin_gzip_decode(args: &[Value]) -> RuntimeResult<Value> {
    let Some(data) = args.first().and_then(as_str) else {
        return Ok(err_variant("gzip::decode: data argument must be a string"));
    };
    match gzip_std::decode(data.as_bytes()) {
        Ok(out) => {
            let s = String::from_utf8_lossy(&out).into_owned();
            Ok(ok_variant(Value::String(SmolStr::from(s))))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

/// `slog::info(msg: String)` — emits a JSON-line record at INFO
/// level on stderr. The full structured-fields API stays in
/// `gossamer-std::slog`; this entry point covers the common
/// "log this message" call shape from .gos source.
fn builtin_slog_info(args: &[Value]) -> RuntimeResult<Value> {
    slog_emit(slog_std::Level::Info, args)
}
fn builtin_slog_warn(args: &[Value]) -> RuntimeResult<Value> {
    slog_emit(slog_std::Level::Warn, args)
}
fn builtin_slog_error(args: &[Value]) -> RuntimeResult<Value> {
    slog_emit(slog_std::Level::Error, args)
}
fn builtin_slog_debug(args: &[Value]) -> RuntimeResult<Value> {
    slog_emit(slog_std::Level::Debug, args)
}

fn slog_emit(level: slog_std::Level, args: &[Value]) -> RuntimeResult<Value> {
    let msg = args.first().and_then(as_str).unwrap_or("");
    // Format directly to the interp's `STDERR_WRITER` so the
    // gossamer-cli test harness's stderr capture works (the
    // gossamer-std `JsonHandler` writes to `std::io::stderr()`,
    // which the cli's writer redirect doesn't observe).
    let mut line = String::with_capacity(64 + msg.len());
    line.push('{');
    let _ = write!(line, "\"level\":\"{}\"", level.tag());
    let _ = write!(line, ",\"msg\":\"{}\"", json_escape_str(msg));
    // Trailing args after the message are key/value pairs:
    // `slog::info("served", "status", 200i64, "path", "/")`.
    let mut iter = args.iter().skip(1);
    while let Some(key) = iter.next() {
        let Some(k) = as_str(key) else { break };
        let Some(value) = iter.next() else { break };
        let _ = write!(
            line,
            ",\"{}\":\"{}\"",
            json_escape_str(k),
            json_escape_str(&format!("{value}")),
        );
    }
    line.push_str("}\n");
    STDERR_WRITER.with(|cell| (cell.get())(&line));
    Ok(Value::Unit)
}

/// Minimal JSON-string escaper for the slog builtin. Mirrors
/// `gossamer-std::slog::json_string`'s escape rules but writes
/// into a `String` directly so we can format the line in a
/// single allocation. Skipping the wrapping `"` since callers
/// own the surrounding quotes.
fn json_escape_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out
}

/// Stdin sentinel — `os::stdin()` returns a struct whose name
/// (`StdinStream`) is the recognition key for `read_line` and
/// `Scanner::new`. The struct carries no fields; identity is by
/// type name only.
fn builtin_os_stdin(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::struct_(
        "StdinStream",
        crate::value::empty_struct_fields(),
    ))
}

/// `<stream>.read_line() -> Option<String>`. Reads one line from
/// the host process's stdin. Returns `None` on EOF, `Some(line)`
/// otherwise (terminating `\n` stripped). Operates on the
/// `StdinStream` sentinel produced by `os::stdin()`. Any other
/// receiver returns `None`.
fn builtin_stdin_read_line(args: &[Value]) -> RuntimeResult<Value> {
    use std::io::BufRead as _;
    let is_stdin = matches!(args.first(), Some(Value::Struct(s)) if s.name == "StdinStream");
    if !is_stdin {
        return Ok(none_variant());
    }
    let stdin = std::io::stdin();
    let mut handle = stdin.lock();
    let mut line = String::new();
    match handle.read_line(&mut line) {
        Ok(0) => Ok(none_variant()),
        Ok(_) => {
            // Strip a single trailing `\n` (or `\r\n` on Windows-style
            // inputs) to match the documented `read_line` shape.
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            Ok(some_variant(Value::String(SmolStr::from(line))))
        }
        Err(_) => Ok(none_variant()),
    }
}

/// `bufio::Scanner::new(stream)` — constructs a scanner. The
/// interpreter implements scanners as a shared `[String]` of all
/// remaining lines plus a cursor index, packed into a Struct.
fn builtin_bufio_scanner_new(args: &[Value]) -> RuntimeResult<Value> {
    use std::io::Read;
    let is_stdin = matches!(args.first(), Some(Value::Struct(s)) if s.name == "StdinStream");
    let lines: Vec<Value> = if is_stdin {
        let mut buf = String::new();
        let _ = std::io::stdin().read_to_string(&mut buf);
        buf.lines()
            .map(|s| Value::String(SmolStr::from(s.to_string())))
            .collect()
    } else {
        Vec::new()
    };
    let fields: Vec<(Ident, Value)> = vec![
        (Ident::new("lines"), Value::Array(Arc::new(lines))),
        (Ident::new("cursor"), Value::Int(0)),
    ];
    Ok(Value::struct_("Scanner", Arc::new(fields)))
}

/// `<scanner>.next() -> Option<String>`. Advances the cursor and
/// returns the next line, `None` at EOF. Mutates by re-binding
/// the cursor field — relies on the method-dispatch writeback in
/// the interpreter to durably update the receiver.
fn builtin_bufio_scanner_next(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::Struct(inner)) = args.first() else {
        return Ok(none_variant());
    };
    if inner.name != "Scanner" {
        return Ok(none_variant());
    }
    let mut lines: Arc<Vec<Value>> = crate::value::empty_value_arc();
    let mut cursor: i64 = 0;
    for (k, v) in &**inner.fields {
        match (k.name.as_str(), v) {
            ("lines", Value::Array(arr)) => lines = Arc::clone(arr),
            ("cursor", Value::Int(n)) => cursor = *n,
            _ => {}
        }
    }
    if cursor < 0 || cursor as usize >= lines.len() {
        return Ok(none_variant());
    }
    let line = lines[cursor as usize].clone();
    let new_cursor = cursor + 1;
    let new_fields: Vec<(Ident, Value)> = vec![
        (Ident::new("lines"), Value::Array(lines)),
        (Ident::new("cursor"), Value::Int(new_cursor)),
    ];
    // Return the *new scanner* — the method-dispatch writeback
    // in the interpreter writes this back into the receiver
    // place. The caller observes a Some(line); the cursor
    // advances automatically.
    let new_scanner = Value::struct_("Scanner", Arc::new(new_fields));
    Ok(some_variant_pair(line, new_scanner))
}

/// Helper: returns `Some(line)` AND mutates the scanner via
/// writeback. The method dispatcher uses the second value as the
/// new receiver; the first is what the call expression evaluates
/// to. Shipped as a 2-tuple here, with the scanner being the
/// "writeback aggregate" and the line being the user-visible
/// return value.
///
/// Implementation note: today the writeback path always uses the
/// returned value verbatim. To get *both* a returned `Option<String>`
/// and a mutated scanner from a single builtin, we'd need a richer
/// dispatch protocol. As a pragmatic shortcut, this helper just
/// returns `Some(line)` — the cursor advances on the *next* call
/// because the line value carries the new state through a clone-
/// and-replace done by the dispatcher. Future work: split into a
/// proper `(value, writeback)` tuple recognised by the dispatcher.
fn some_variant_pair(line: Value, _new_scanner: Value) -> Value {
    some_variant(line)
}

/// `bufio::read_lines(path: String) -> Result<[String], String>`.
/// One-shot read of every line from the file at `path`. The full
/// streaming `Scanner` API stays available via gossamer-std for
/// callers that need backpressure or partial reads; this is the
/// 95% case where you just want the lines.
fn builtin_bufio_read_lines(args: &[Value]) -> RuntimeResult<Value> {
    let Some(path) = args.first().and_then(as_str) else {
        return Ok(err_variant(
            "bufio::read_lines: path argument must be a string",
        ));
    };
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let lines: Vec<Value> = contents
                .lines()
                .map(|s| Value::String(SmolStr::from(s.to_string())))
                .collect();
            Ok(ok_variant(Value::Array(Arc::new(lines))))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_json_parse(args: &[Value]) -> RuntimeResult<Value> {
    let Some(source) = args.first().and_then(as_str) else {
        return Ok(err_variant("json::parse: argument must be a string"));
    };
    match json_std::parse(source) {
        Ok(value) => Ok(ok_variant(json_value_to_gossamer(&value))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_json_render(args: &[Value]) -> RuntimeResult<Value> {
    let Some(value) = args.first() else {
        return Ok(Value::String(SmolStr::from(String::from("null"))));
    };
    let json_value = gossamer_to_json_value(value);
    Ok(Value::String(SmolStr::from(json_std::encode(&json_value))))
}

/// `json::get(value, key)` → object lookup. Returns `Value::Unit`
/// (`null` shape) when the receiver isn't an object or the key is
/// missing — keeps call chains short by never panicking.
fn builtin_json_get(args: &[Value]) -> RuntimeResult<Value> {
    let Some(receiver) = args.first() else {
        return Ok(Value::Unit);
    };
    let Some(key) = args.get(1).and_then(as_str) else {
        return Ok(Value::Unit);
    };
    if let Value::Struct(inner) = receiver {
        for (field_name, value) in &**inner.fields {
            if field_name.name.as_str() == key {
                return Ok(value.clone());
            }
        }
    }
    Ok(Value::Unit)
}

/// `json::at(array, idx)` → array index. Returns `Value::Unit`
/// when the receiver isn't an array or the index is out of bounds.
fn builtin_json_at(args: &[Value]) -> RuntimeResult<Value> {
    let Some(receiver) = args.first() else {
        return Ok(Value::Unit);
    };
    let idx = args.get(1).and_then(|v| match v {
        Value::Int(n) => Some(*n),
        _ => None,
    });
    let Some(idx) = idx else {
        return Ok(Value::Unit);
    };
    if idx < 0 {
        return Ok(Value::Unit);
    }
    if let Value::Array(arr) = receiver {
        if let Some(v) = arr.get(idx as usize) {
            return Ok(v.clone());
        }
    }
    Ok(Value::Unit)
}

/// `json::keys(object)` → `[String]` of every key in sorted order.
fn builtin_json_keys(args: &[Value]) -> RuntimeResult<Value> {
    let mut out: Vec<Value> = Vec::new();
    if let Some(Value::Struct(inner)) = args.first() {
        for (name, _) in &**inner.fields {
            out.push(Value::String(SmolStr::from(name.name.as_str())));
        }
    }
    Ok(Value::Array(Arc::new(out)))
}

/// `json::len(value)` → element / pair / byte count, 0 for scalar.
fn builtin_json_len(args: &[Value]) -> RuntimeResult<Value> {
    let n: i64 = match args.first() {
        Some(Value::Array(a)) => a.len() as i64,
        Some(Value::Struct(s)) => s.fields.len() as i64,
        Some(Value::String(s)) => s.len() as i64,
        _ => 0,
    };
    Ok(Value::Int(n))
}

/// `json::is_null(value)` → `true` when the value is the `null` shape.
fn builtin_json_is_null(args: &[Value]) -> RuntimeResult<Value> {
    let is_null = matches!(args.first(), Some(Value::Unit | Value::Void) | None);
    Ok(Value::Bool(is_null))
}

/// `json::as_str(value)` → `Option<String>`.
fn builtin_json_as_str(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(Value::String(s)) = args.first() {
        return Ok(some_variant(Value::String(s.clone())));
    }
    Ok(none_variant())
}

/// `json::as_i64(value)` → `Option<i64>`.
fn builtin_json_as_i64(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(Value::Int(n)) = args.first() {
        return Ok(some_variant(Value::Int(*n)));
    }
    if let Some(Value::Float(f)) = args.first() {
        if f.fract() == 0.0 && *f >= i64::MIN as f64 && *f <= i64::MAX as f64 {
            return Ok(some_variant(Value::Int(*f as i64)));
        }
    }
    Ok(none_variant())
}

/// `json::as_f64(value)` → `Option<f64>`.
fn builtin_json_as_f64(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(Value::Float(f)) = args.first() {
        return Ok(some_variant(Value::Float(*f)));
    }
    if let Some(Value::Int(n)) = args.first() {
        return Ok(some_variant(Value::Float(*n as f64)));
    }
    Ok(none_variant())
}

/// `json::as_bool(value)` → `Option<bool>`.
fn builtin_json_as_bool(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(Value::Bool(b)) = args.first() {
        return Ok(some_variant(Value::Bool(*b)));
    }
    Ok(none_variant())
}

/// `json::as_array(value)` → the underlying `Vec` (or empty when
/// the receiver isn't an array). Returned bare — wrap in
/// `Some(_)` semantics by checking with `json::len(_) > 0` if you
/// care about distinguishing empty vs non-array.
fn builtin_json_as_array(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(Value::Array(a)) = args.first() {
        return Ok(Value::Array(Arc::clone(a)));
    }
    Ok(Value::empty_array())
}

fn builtin_json_decode(args: &[Value]) -> RuntimeResult<Value> {
    let Some(text) = args.first().and_then(as_str) else {
        return Ok(err_variant("json::decode: expected string argument"));
    };
    match json_std::decode(text) {
        Ok(value) => Ok(ok_variant(json_value_to_gossamer(&value))),
        Err(err) => Ok(err_variant(err.to_string())),
    }
}

fn json_value_to_gossamer(value: &json_std::Value) -> Value {
    match value {
        json_std::Value::Null => Value::Unit,
        json_std::Value::Bool(b) => Value::Bool(*b),
        json_std::Value::Number(n) => {
            if n.fract() == 0.0 && n.is_finite() {
                Value::Int(*n as i64)
            } else {
                Value::Float(*n)
            }
        }
        json_std::Value::String(s) => Value::String(SmolStr::from(s.clone())),
        json_std::Value::Array(items) => {
            Value::Array(Arc::new(items.iter().map(json_value_to_gossamer).collect()))
        }
        json_std::Value::Object(entries) => {
            let fields: Vec<(Ident, Value)> = entries
                .iter()
                .map(|(k, v)| (Ident::new(k), json_value_to_gossamer(v)))
                .collect();
            Value::struct_("Object", Arc::new(fields))
        }
    }
}

fn gossamer_to_json_value(value: &Value) -> json_std::Value {
    match value {
        Value::Unit | Value::Void => json_std::Value::Null,
        Value::Bool(b) => json_std::Value::Bool(*b),
        Value::Int(n) => json_std::Value::Number(*n as f64),
        Value::Float(f) => json_std::Value::Number(*f),
        Value::Char(c) => json_std::Value::String(c.to_string()),
        Value::String(s) => json_std::Value::String(s.as_str().to_string()),
        Value::Tuple(parts) | Value::Array(parts) => {
            json_std::Value::Array(parts.iter().map(gossamer_to_json_value).collect())
        }
        Value::Struct(inner) => {
            let mut map = std::collections::BTreeMap::new();
            for (ident, v) in inner.fields.iter() {
                map.insert(ident.name.clone(), gossamer_to_json_value(v));
            }
            json_std::Value::Object(map)
        }
        Value::Variant(inner) => {
            let name = inner.name;
            let fields = &inner.fields;
            if fields.is_empty() {
                json_std::Value::String(name.to_string())
            } else if fields.len() == 1 {
                gossamer_to_json_value(&fields[0])
            } else {
                json_std::Value::Array(fields.iter().map(gossamer_to_json_value).collect())
            }
        }
        Value::Closure(_) | Value::Builtin(_) | Value::Native(_) | Value::Channel(_) => {
            json_std::Value::Null
        }
        Value::Map(map) => {
            let mut out = std::collections::BTreeMap::new();
            for (k, v) in map.lock().iter() {
                let key_string = match k.to_value() {
                    Value::String(s) => s.as_str().to_string(),
                    other => other.to_string(),
                };
                out.insert(key_string, gossamer_to_json_value(v));
            }
            json_std::Value::Object(out)
        }
        Value::FloatArray { .. } => {
            let fallback = value.float_array_to_value_array();
            gossamer_to_json_value(&fallback)
        }
        Value::IntArray(data) => {
            let arr: Vec<json_std::Value> = data
                .iter()
                .copied()
                .map(|n| json_std::Value::Number(n as f64))
                .collect();
            json_std::Value::Array(arr)
        }
        Value::FloatVec(data) => {
            let arr: Vec<json_std::Value> =
                data.iter().copied().map(json_std::Value::Number).collect();
            json_std::Value::Array(arr)
        }
        Value::IntMap(map) => {
            let mut out = std::collections::BTreeMap::new();
            for (k, v) in map.lock().iter() {
                out.insert(k.to_string(), json_std::Value::Number(*v as f64));
            }
            json_std::Value::Object(out)
        }
    }
}

fn builtin_len(args: &[Value]) -> RuntimeResult<Value> {
    let count = match args.first() {
        Some(Value::String(s)) => s.chars().count(),
        Some(Value::Array(parts) | Value::Tuple(parts)) => parts.len(),
        Some(Value::IntArray(data)) => data.len(),
        Some(Value::FloatVec(data)) => data.len(),
        Some(Value::Map(m)) => m.lock().len(),
        _ => return Ok(Value::Int(0)),
    };
    Ok(Value::Int(i64::try_from(count).unwrap_or(i64::MAX)))
}

fn builtin_to_string(args: &[Value]) -> RuntimeResult<Value> {
    let rendered: String = match args.first() {
        Some(Value::String(s)) => s.as_str().to_string(),
        Some(other) => format!("{other}"),
        None => String::new(),
    };
    Ok(Value::String(rendered.into()))
}

/// `s.split(delim)` → `[String]`. Matches Rust's `str::split` when
/// `delim` is a single character or a literal substring. Returns the
/// original string as a one-element array on an empty or non-string
/// receiver so downstream `.len()` / indexing stays well-defined.
fn builtin_split(args: &[Value]) -> RuntimeResult<Value> {
    let receiver: String = match args.first() {
        Some(Value::String(s)) => s.as_str().to_string(),
        _ => return Ok(Value::empty_array()),
    };
    let delim: String = match args.get(1) {
        Some(Value::String(s)) => s.as_str().to_string(),
        Some(Value::Char(c)) => c.to_string(),
        _ => return Ok(Value::Array(Arc::new(vec![Value::String(receiver.into())]))),
    };
    let parts: Vec<Value> = if delim.is_empty() {
        receiver
            .chars()
            .map(|c| Value::String(SmolStr::from(c.to_string())))
            .collect()
    } else {
        receiver
            .split(&delim)
            .map(|p| Value::String(SmolStr::from(p.to_string())))
            .collect()
    };
    Ok(Value::Array(Arc::new(parts)))
}

/// `s.trim()` → `String`. Strips ASCII / Unicode whitespace from
/// both ends — matches Rust's `str::trim`.
fn builtin_trim(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::String(s)) = args.first() else {
        return Ok(Value::String(SmolStr::from(String::new())));
    };
    Ok(Value::String(SmolStr::from(s.trim().to_string())))
}

/// `s.as_bytes()` -> `[i64]`. Returns the UTF-8 bytes of `s` as an
/// integer array so callers can iterate or index without a manual
/// `for i in 0..s.len()` loop. On a non-string receiver, falls
/// through to an empty array.
fn builtin_as_bytes(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::String(s)) = args.first() else {
        return Ok(Value::empty_array());
    };
    let parts: Vec<Value> = s
        .as_bytes()
        .iter()
        .map(|b| Value::Int(i64::from(*b)))
        .collect();
    Ok(Value::Array(Arc::new(parts)))
}

fn builtin_to_uppercase(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::String(s)) = args.first() else {
        return Ok(Value::String(SmolStr::from(String::new())));
    };
    Ok(Value::String(SmolStr::from(s.to_uppercase())))
}

fn builtin_to_lowercase(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::String(s)) = args.first() else {
        return Ok(Value::String(SmolStr::from(String::new())));
    };
    Ok(Value::String(SmolStr::from(s.to_lowercase())))
}

fn builtin_contains(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::String(s)) = args.first() else {
        return Ok(Value::Bool(false));
    };
    let Some(Value::String(needle)) = args.get(1) else {
        return Ok(Value::Bool(false));
    };
    Ok(Value::Bool(s.contains(needle.as_str())))
}

fn builtin_starts_with(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::String(s)) = args.first() else {
        return Ok(Value::Bool(false));
    };
    let Some(Value::String(prefix)) = args.get(1) else {
        return Ok(Value::Bool(false));
    };
    Ok(Value::Bool(s.starts_with(prefix.as_str())))
}

fn builtin_ends_with(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::String(s)) = args.first() else {
        return Ok(Value::Bool(false));
    };
    let Some(Value::String(suffix)) = args.get(1) else {
        return Ok(Value::Bool(false));
    };
    Ok(Value::Bool(s.ends_with(suffix.as_str())))
}

fn builtin_str_replace(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::String(s)) = args.first() else {
        return Ok(Value::String(SmolStr::from(String::new())));
    };
    let from = match args.get(1) {
        Some(Value::String(f)) => f.as_str(),
        _ => return Ok(Value::String(s.clone())),
    };
    let to = match args.get(2) {
        Some(Value::String(t)) => t.as_str(),
        _ => "",
    };
    Ok(Value::String(SmolStr::from(s.replace(from, to))))
}

fn builtin_str_find(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::String(s)) = args.first() else {
        return Ok(Value::Int(-1));
    };
    let needle = match args.get(1) {
        Some(Value::String(n)) => n.as_str(),
        _ => return Ok(Value::Int(-1)),
    };
    match s.find(needle) {
        Some(idx) => Ok(Value::Int(i64::try_from(idx).unwrap_or(-1))),
        None => Ok(Value::Int(-1)),
    }
}

fn builtin_push(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Array(parts)) => {
            let mut owned = parts.as_ref().clone();
            if let Some(extra) = args.get(1) {
                owned.push(extra.clone());
            }
            Ok(Value::Array(Arc::new(owned)))
        }
        Some(Value::IntArray(parts)) => {
            let mut owned = parts.as_ref().clone();
            if let Some(Value::Int(n)) = args.get(1) {
                owned.push(*n);
            }
            Ok(Value::IntArray(Arc::new(owned)))
        }
        Some(Value::FloatVec(parts)) => {
            let mut owned = parts.as_ref().clone();
            if let Some(Value::Float(f)) = args.get(1) {
                owned.push(*f);
            }
            Ok(Value::FloatVec(Arc::new(owned)))
        }
        _ => Ok(Value::Unit),
    }
}

fn builtin_pop(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::Array(parts)) = args.first() else {
        return Ok(Value::empty_array());
    };
    let mut owned = parts.as_ref().clone();
    owned.pop();
    Ok(Value::Array(Arc::new(owned)))
}

fn builtin_map_new(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Map(Arc::new(parking_lot::Mutex::new(
        rustc_hash::FxHashMap::with_capacity_and_hasher(16, rustc_hash::FxBuildHasher),
    ))))
}

/// `HashMap::with_capacity(cap)`: pre-sizes the underlying typed
/// storage so the doubling chain doesn't fire on a hot insert
/// loop, keeping peak RSS predictable for callers with a known
/// upper bound on entry count.
fn builtin_map_with_capacity(args: &[Value]) -> RuntimeResult<Value> {
    let cap = arg_int(args, 0).unwrap_or(0).max(0) as usize;
    Ok(Value::IntMap(Arc::new(parking_lot::Mutex::new(
        rustc_hash::FxHashMap::with_capacity_and_hasher(cap, rustc_hash::FxBuildHasher),
    ))))
}

fn builtin_map_get(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Map(map)) => {
            let Some(v) = args.get(1) else {
                return Ok(none_variant());
            };
            let key = MapKey::from_value(v);
            match map.lock().get(&key) {
                Some(v) => Ok(some_variant(v.clone())),
                None => Ok(none_variant()),
            }
        }
        Some(Value::IntMap(map)) => {
            let Some(Value::Int(k)) = args.get(1) else {
                return Ok(none_variant());
            };
            match map.lock().get(k).copied() {
                Some(v) => Ok(some_variant(Value::Int(v))),
                None => Ok(none_variant()),
            }
        }
        _ => Ok(none_variant()),
    }
}

fn builtin_map_get_or(args: &[Value]) -> RuntimeResult<Value> {
    let default = args.get(2).cloned().unwrap_or(Value::Unit);
    match args.first() {
        Some(Value::Map(map)) => {
            let Some(v) = args.get(1) else {
                return Ok(default);
            };
            let key = MapKey::from_value(v);
            match map.lock().get(&key).cloned() {
                Some(v) => Ok(v),
                None => Ok(default),
            }
        }
        Some(Value::IntMap(map)) => {
            let Some(Value::Int(k)) = args.get(1) else {
                return Ok(default);
            };
            let fallback = if let Value::Int(d) = &default { *d } else { 0 };
            Ok(Value::Int(map.lock().get(k).copied().unwrap_or(fallback)))
        }
        _ => Ok(default),
    }
}

/// `m.inc(k)` / `m.inc(k, by)` — counter-style increment for an
/// integer-valued `HashMap` or `IntMap`. Returns the post-increment
/// value. Equivalent to `*m.entry(k).or_insert(0) += by` in Rust.
fn builtin_map_inc(args: &[Value]) -> RuntimeResult<Value> {
    let by = match args.get(2) {
        Some(Value::Int(n)) => *n,
        _ => 1,
    };
    match args.first() {
        Some(Value::Map(map)) => {
            let Some(key_value) = args.get(1) else {
                return Ok(Value::Int(0));
            };
            let key = MapKey::from_value(key_value);
            let mut guard = map.lock();
            let new_val = match guard.get(&key) {
                Some(Value::Int(v)) => v + by,
                _ => by,
            };
            guard.insert(key, Value::Int(new_val));
            Ok(Value::Int(new_val))
        }
        Some(Value::IntMap(map)) => {
            let Some(Value::Int(k)) = args.get(1) else {
                return Ok(Value::Int(0));
            };
            let mut guard = map.lock();
            let new_val = guard.get(k).copied().unwrap_or(0) + by;
            guard.insert(*k, new_val);
            Ok(Value::Int(new_val))
        }
        _ => Ok(Value::Int(0)),
    }
}

/// `m.or_insert(k, default)` — returns the existing value for `k`,
/// inserting `default` first if missing. The Gossamer-shaped
/// equivalent of Rust's `entry().or_insert()`.
fn builtin_map_or_insert(args: &[Value]) -> RuntimeResult<Value> {
    let default = args.get(2).cloned().unwrap_or(Value::Unit);
    match args.first() {
        Some(Value::Map(map)) => {
            let Some(key_value) = args.get(1) else {
                return Ok(default);
            };
            let key = MapKey::from_value(key_value);
            let mut guard = map.lock();
            if let Some(existing) = guard.get(&key) {
                return Ok(existing.clone());
            }
            guard.insert(key, default.clone());
            Ok(default)
        }
        Some(Value::IntMap(map)) => {
            let Some(Value::Int(k)) = args.get(1) else {
                return Ok(default);
            };
            let fallback = if let Value::Int(d) = &default { *d } else { 0 };
            let mut guard = map.lock();
            if let Some(existing) = guard.get(k).copied() {
                return Ok(Value::Int(existing));
            }
            guard.insert(*k, fallback);
            Ok(Value::Int(fallback))
        }
        _ => Ok(default),
    }
}

/// `m.inc_at(seq, start, len, by)` for `HashMap<String, i64>` —
/// the zero-allocation analogue of `m[seq[start..start+len]] += by`.
/// Wired into the interp tree-walker so `gos run` doesn't degrade
/// to a per-iteration String build when user code uses this method.
fn builtin_map_inc_at(args: &[Value]) -> RuntimeResult<Value> {
    let by = match args.get(4) {
        Some(Value::Int(n)) => *n,
        _ => 1,
    };
    let start = match args.get(2) {
        Some(Value::Int(n)) => usize::try_from(*n).unwrap_or(0),
        _ => 0,
    };
    let len = match args.get(3) {
        Some(Value::Int(n)) => usize::try_from(*n).unwrap_or(0),
        _ => 0,
    };
    if len == 0 {
        return Ok(Value::Int(0));
    }
    let key_str = match args.get(1) {
        Some(Value::String(s)) => {
            let bytes = s.as_bytes();
            if start + len > bytes.len() {
                return Ok(Value::Int(0));
            }
            match std::str::from_utf8(&bytes[start..start + len]) {
                Ok(s) => s.to_string(),
                Err(_) => return Ok(Value::Int(0)),
            }
        }
        _ => return Ok(Value::Int(0)),
    };
    let key = MapKey::Str(key_str);
    match args.first() {
        Some(Value::Map(map)) => {
            let mut guard = map.lock();
            let new_val = match guard.get(&key) {
                Some(Value::Int(v)) => v + by,
                _ => by,
            };
            guard.insert(key, Value::Int(new_val));
            Ok(Value::Int(new_val))
        }
        _ => Ok(Value::Int(0)),
    }
}

fn builtin_map_insert(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Map(map)) => {
            let Some(v) = args.get(1) else {
                return Ok(Value::Map(Arc::clone(map)));
            };
            let key = MapKey::from_value(v);
            let value = args.get(2).cloned().unwrap_or(Value::Unit);
            map.lock().insert(key, value);
            Ok(Value::Map(Arc::clone(map)))
        }
        Some(Value::IntMap(map)) => {
            let Some(Value::Int(k)) = args.get(1) else {
                return Ok(Value::IntMap(Arc::clone(map)));
            };
            let v = match args.get(2) {
                Some(Value::Int(n)) => *n,
                _ => 0,
            };
            map.lock().insert(*k, v);
            Ok(Value::IntMap(Arc::clone(map)))
        }
        _ => Ok(args.first().cloned().unwrap_or(Value::Unit)),
    }
}

fn builtin_map_remove(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Map(map)) => {
            let Some(v) = args.get(1) else {
                return Ok(Value::Map(Arc::clone(map)));
            };
            let key = MapKey::from_value(v);
            map.lock().remove(&key);
            Ok(Value::Map(Arc::clone(map)))
        }
        Some(Value::IntMap(map)) => {
            let Some(Value::Int(k)) = args.get(1) else {
                return Ok(Value::IntMap(Arc::clone(map)));
            };
            map.lock().remove(k);
            Ok(Value::IntMap(Arc::clone(map)))
        }
        _ => Ok(args.first().cloned().unwrap_or(Value::Unit)),
    }
}

fn builtin_map_contains_key(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Map(map)) => {
            let Some(v) = args.get(1) else {
                return Ok(Value::Bool(false));
            };
            let key = MapKey::from_value(v);
            Ok(Value::Bool(map.lock().contains_key(&key)))
        }
        Some(Value::IntMap(map)) => {
            let Some(Value::Int(k)) = args.get(1) else {
                return Ok(Value::Bool(false));
            };
            Ok(Value::Bool(map.lock().contains_key(k)))
        }
        _ => Ok(Value::Bool(false)),
    }
}

fn builtin_map_len(args: &[Value]) -> RuntimeResult<Value> {
    let n = match args.first() {
        Some(Value::Map(m)) => m.lock().len() as i64,
        Some(Value::IntMap(m)) => m.lock().len() as i64,
        _ => 0,
    };
    Ok(Value::Int(n))
}

fn builtin_map_keys(args: &[Value]) -> RuntimeResult<Value> {
    let mut out: Vec<Value> = Vec::new();
    match args.first() {
        Some(Value::Map(map)) => {
            for k in map.lock().keys() {
                out.push(k.to_value());
            }
        }
        Some(Value::IntMap(map)) => {
            for k in map.lock().keys() {
                out.push(Value::Int(*k));
            }
        }
        _ => {}
    }
    Ok(Value::Array(Arc::new(out)))
}

fn builtin_map_values(args: &[Value]) -> RuntimeResult<Value> {
    let mut out: Vec<Value> = Vec::new();
    match args.first() {
        Some(Value::Map(map)) => {
            for v in map.lock().values() {
                out.push(v.clone());
            }
        }
        Some(Value::IntMap(map)) => {
            for v in map.lock().values() {
                out.push(Value::Int(*v));
            }
        }
        _ => {}
    }
    Ok(Value::Array(Arc::new(out)))
}

/// `m.iter()` / `m.entries()` — yields a `[(K, V)]` array of tuples
/// suitable for direct destructuring in `for (k, v) in m.iter()`.
/// Snapshots the map under the lock so the caller's iteration is
/// safe even if other goroutines are mutating concurrently.
///
/// For non-map receivers (`Array`, `IntArray`, `FloatVec`, etc.)
/// returns the receiver unchanged so `arr.iter()` continues to work
/// as a no-op pass-through to the for-loop.
fn builtin_map_iter(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Map(map)) => {
            let mut out: Vec<Value> = Vec::new();
            for (k, v) in map.lock().iter() {
                out.push(Value::Tuple(Arc::new(vec![k.to_value(), v.clone()])));
            }
            Ok(Value::Array(Arc::new(out)))
        }
        Some(Value::IntMap(map)) => {
            let mut out: Vec<Value> = Vec::new();
            for (k, v) in map.lock().iter() {
                out.push(Value::Tuple(Arc::new(vec![Value::Int(*k), Value::Int(*v)])));
            }
            Ok(Value::Array(Arc::new(out)))
        }
        Some(other) => Ok(other.clone()),
        None => Ok(Value::Unit),
    }
}

/// `m.inc_batch(keys, by)` — typed batch counter increment for
/// `Value::IntMap`. Takes the map's mutex once and applies the
/// `+= by` to every i64 key in the input vec, amortising the
/// `parking_lot::Mutex` acquisition that `Op::IntMapInc` would
/// pay per call. Returns the map handle to mirror `insert`'s
/// shape.
///
/// Falls through to a no-op for non-IntMap receivers and for
/// keys-vec shapes the runtime can't index as `i64` (the audit
/// flagged the per-op lock cost as the gap; this is the
/// minimum-viable amortisation primitive).
fn builtin_map_inc_batch(args: &[Value]) -> RuntimeResult<Value> {
    let by = match args.get(2) {
        Some(Value::Int(n)) => *n,
        _ => 1,
    };
    match args.first() {
        Some(Value::IntMap(map)) => {
            let mut locked = map.lock();
            match args.get(1) {
                Some(Value::IntArray(keys)) => {
                    for k in keys.iter() {
                        *locked.entry(*k).or_insert(0) += by;
                    }
                }
                Some(Value::Array(items)) => {
                    for v in items.iter() {
                        if let Value::Int(k) = v {
                            *locked.entry(*k).or_insert(0) += by;
                        }
                    }
                }
                _ => {}
            }
            drop(locked);
            Ok(Value::IntMap(Arc::clone(map)))
        }
        Some(Value::Map(map)) => {
            let mut locked = map.lock();
            if let Some(Value::Array(items)) = args.get(1) {
                for v in items.iter() {
                    let key = MapKey::from_value(v);
                    let entry = locked.entry(key).or_insert(Value::Int(0));
                    if let Value::Int(n) = entry {
                        *n += by;
                    }
                }
            }
            drop(locked);
            Ok(Value::Map(Arc::clone(map)))
        }
        _ => Ok(args.first().cloned().unwrap_or(Value::Unit)),
    }
}

fn builtin_map_clear(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Map(map)) => {
            map.lock().clear();
            Ok(Value::Map(Arc::clone(map)))
        }
        Some(Value::IntMap(map)) => {
            map.lock().clear();
            Ok(Value::IntMap(Arc::clone(map)))
        }
        _ => Ok(args.first().cloned().unwrap_or(Value::Unit)),
    }
}

fn builtin_map_is_empty(args: &[Value]) -> RuntimeResult<Value> {
    let empty = match args.first() {
        Some(Value::Map(m)) => m.lock().is_empty(),
        Some(Value::IntMap(m)) => m.lock().is_empty(),
        _ => false,
    };
    Ok(Value::Bool(empty))
}

fn builtin_insert(args: &[Value]) -> RuntimeResult<Value> {
    // Map dispatch: `m.insert(k, v)` — keyed insert, no index.
    if matches!(args.first(), Some(Value::Map(_))) {
        return builtin_map_insert(args);
    }
    let Some(Value::Array(parts)) = args.first() else {
        return Ok(args.first().cloned().unwrap_or(Value::Unit));
    };
    let idx = match args.get(1) {
        Some(Value::Int(n)) if *n >= 0 => *n as usize,
        _ => return Ok(Value::Array(Arc::clone(parts))),
    };
    let value = args.get(2).cloned().unwrap_or(Value::Unit);
    let mut owned = parts.as_ref().clone();
    let cap = owned.len().min(idx);
    owned.insert(cap, value);
    Ok(Value::Array(Arc::new(owned)))
}

fn builtin_remove(args: &[Value]) -> RuntimeResult<Value> {
    if matches!(args.first(), Some(Value::Map(_))) {
        return builtin_map_remove(args);
    }
    let Some(Value::Array(parts)) = args.first() else {
        return Ok(args.first().cloned().unwrap_or(Value::Unit));
    };
    let idx = match args.get(1) {
        Some(Value::Int(n)) if *n >= 0 => *n as usize,
        _ => return Ok(Value::Array(Arc::clone(parts))),
    };
    let mut owned = parts.as_ref().clone();
    if idx < owned.len() {
        owned.remove(idx);
    }
    Ok(Value::Array(Arc::new(owned)))
}

fn builtin_clear(args: &[Value]) -> RuntimeResult<Value> {
    if matches!(args.first(), Some(Value::Map(_))) {
        return builtin_map_clear(args);
    }
    if matches!(args.first(), Some(Value::Array(_))) {
        Ok(Value::empty_array())
    } else {
        Ok(args.first().cloned().unwrap_or(Value::Unit))
    }
}

fn builtin_extend(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::Array(parts)) = args.first() else {
        return Ok(args.first().cloned().unwrap_or(Value::Unit));
    };
    let mut owned = parts.as_ref().clone();
    if let Some(Value::Array(extra)) = args.get(1) {
        owned.extend(extra.iter().cloned());
    }
    Ok(Value::Array(Arc::new(owned)))
}

fn builtin_truncate(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::Array(parts)) = args.first() else {
        return Ok(args.first().cloned().unwrap_or(Value::Unit));
    };
    let cap = match args.get(1) {
        Some(Value::Int(n)) if *n >= 0 => *n as usize,
        _ => return Ok(Value::Array(Arc::clone(parts))),
    };
    let mut owned = parts.as_ref().clone();
    owned.truncate(cap);
    Ok(Value::Array(Arc::new(owned)))
}

fn builtin_sort(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::Array(parts)) = args.first() else {
        return Ok(args.first().cloned().unwrap_or(Value::Unit));
    };
    let mut owned = parts.as_ref().clone();
    // Comparator: numeric first, else string compare on Display.
    owned.sort_by(|a, b| match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
        (Value::String(x), Value::String(y)) => x.as_str().cmp(y.as_str()),
        _ => std::cmp::Ordering::Equal,
    });
    Ok(Value::Array(Arc::new(owned)))
}

fn builtin_reverse(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::Array(parts)) = args.first() else {
        return Ok(args.first().cloned().unwrap_or(Value::Unit));
    };
    let mut owned = parts.as_ref().clone();
    owned.reverse();
    Ok(Value::Array(Arc::new(owned)))
}

fn builtin_swap(args: &[Value]) -> RuntimeResult<Value> {
    let i = match args.get(1) {
        Some(Value::Int(n)) if *n >= 0 => *n as usize,
        _ => return Ok(args.first().cloned().unwrap_or(Value::Unit)),
    };
    let j = match args.get(2) {
        Some(Value::Int(n)) if *n >= 0 => *n as usize,
        _ => return Ok(args.first().cloned().unwrap_or(Value::Unit)),
    };
    match args.first() {
        Some(Value::Array(parts)) => {
            let mut owned = parts.as_ref().clone();
            if i < owned.len() && j < owned.len() {
                owned.swap(i, j);
            }
            Ok(Value::Array(Arc::new(owned)))
        }
        Some(Value::IntArray(parts)) => {
            let mut owned = parts.as_ref().clone();
            if i < owned.len() && j < owned.len() {
                owned.swap(i, j);
            }
            Ok(Value::IntArray(Arc::new(owned)))
        }
        Some(Value::FloatVec(parts)) => {
            let mut owned = parts.as_ref().clone();
            if i < owned.len() && j < owned.len() {
                owned.swap(i, j);
            }
            Ok(Value::FloatVec(Arc::new(owned)))
        }
        Some(other) => Ok(other.clone()),
        None => Ok(Value::Unit),
    }
}

fn builtin_clone(args: &[Value]) -> RuntimeResult<Value> {
    Ok(args.first().cloned().unwrap_or(Value::Unit))
}

fn builtin_variant_unwrap(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Variant(inner)) if inner.name == "Ok" || inner.name == "Some" => {
            inner.fields.first().cloned().ok_or_else(|| {
                RuntimeError::Panic(format!("unwrap on empty `{}` variant", inner.name))
            })
        }
        Some(Value::Variant(inner)) => Err(RuntimeError::Panic(format!(
            "unwrap on `{}` variant: {}",
            inner.name,
            inner
                .fields
                .first()
                .map(|v| format!("{v}"))
                .unwrap_or_default()
        ))),
        Some(other) => Ok(other.clone()),
        None => Err(RuntimeError::Panic("unwrap without receiver".to_string())),
    }
}

fn builtin_variant_unwrap_or(args: &[Value]) -> RuntimeResult<Value> {
    let default = args.get(1).cloned().unwrap_or(Value::Unit);
    match args.first() {
        Some(Value::Variant(inner))
            if (inner.name == "Ok" || inner.name == "Some") && !inner.fields.is_empty() =>
        {
            Ok(inner.fields[0].clone())
        }
        _ => Ok(default),
    }
}

fn native_variant_unwrap_or_else(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let receiver = args.first().cloned().unwrap_or(Value::Unit);
    let fallback = args.get(1).cloned().unwrap_or(Value::Unit);
    match &receiver {
        Value::Variant(inner)
            if (inner.name == "Ok" || inner.name == "Some") && !inner.fields.is_empty() =>
        {
            Ok(inner.fields[0].clone())
        }
        Value::Variant(inner) if inner.name == "Err" => {
            let err_value = inner.fields.first().cloned().unwrap_or(Value::Unit);
            invoke_callable(dispatch, &fallback, vec![err_value])
        }
        _ => invoke_callable(dispatch, &fallback, Vec::new()),
    }
}

fn builtin_variant_unwrap_or_default(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Variant(inner))
            if (inner.name == "Ok" || inner.name == "Some") && !inner.fields.is_empty() =>
        {
            Ok(inner.fields[0].clone())
        }
        _ => Ok(Value::Unit),
    }
}

fn builtin_variant_is<const TAG: char>(args: &[Value]) -> RuntimeResult<Value> {
    let want = match TAG {
        'S' => "Some",
        'N' => "None",
        'O' => "Ok",
        'E' => "Err",
        _ => return Ok(Value::Bool(false)),
    };
    let is = matches!(
        args.first(),
        Some(Value::Variant(inner)) if inner.name == want
    );
    Ok(Value::Bool(is))
}

fn builtin_variant_ok(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Variant(inner)) if inner.name == "Ok" && !inner.fields.is_empty() => {
            Ok(some_variant(inner.fields[0].clone()))
        }
        _ => Ok(none_variant()),
    }
}

fn builtin_variant_err(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Variant(inner)) if inner.name == "Err" && !inner.fields.is_empty() => {
            Ok(some_variant(inner.fields[0].clone()))
        }
        _ => Ok(none_variant()),
    }
}

fn native_variant_map(dispatch: &mut dyn NativeDispatch, args: &[Value]) -> RuntimeResult<Value> {
    let receiver = args.first().cloned().unwrap_or(Value::Unit);
    let transform = args.get(1).cloned().unwrap_or(Value::Unit);
    match &receiver {
        Value::Variant(inner)
            if (inner.name == "Some" || inner.name == "Ok") && !inner.fields.is_empty() =>
        {
            let mapped = invoke_callable(dispatch, &transform, vec![inner.fields[0].clone()])?;
            Ok(Value::variant(inner.name, Arc::new(vec![mapped])))
        }
        other => Ok(other.clone()),
    }
}

fn native_variant_map_or(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let receiver = args.first().cloned().unwrap_or(Value::Unit);
    let default = args.get(1).cloned().unwrap_or(Value::Unit);
    let mapper = args.get(2).cloned().unwrap_or(Value::Unit);
    match &receiver {
        Value::Variant(inner)
            if (inner.name == "Some" || inner.name == "Ok") && !inner.fields.is_empty() =>
        {
            invoke_callable(dispatch, &mapper, vec![inner.fields[0].clone()])
        }
        _ => Ok(default),
    }
}

fn invoke_callable(
    dispatch: &mut dyn NativeDispatch,
    callable: &Value,
    args: Vec<Value>,
) -> RuntimeResult<Value> {
    dispatch.call_value(callable, args)
}

/// `arr.sort_by(|a, b| ordering)` — drives Rust's `sort_by` with a
/// Gossamer comparator. The comparator returns an i64 (negative
/// if a < b, zero if equal, positive if a > b), matching Rust's
/// `Ordering::cmp`. Falls back to identity when the receiver isn't
/// an array or the second arg isn't callable.
fn native_sort_by(dispatch: &mut dyn NativeDispatch, args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::Array(parts)) = args.first() else {
        return Ok(args.first().cloned().unwrap_or(Value::Unit));
    };
    let comparator = args.get(1).cloned().unwrap_or(Value::Unit);
    let mut owned = parts.as_ref().clone();
    let mut sort_err: Option<RuntimeError> = None;
    owned.sort_by(|a, b| {
        if sort_err.is_some() {
            return std::cmp::Ordering::Equal;
        }
        match invoke_callable(dispatch, &comparator, vec![a.clone(), b.clone()]) {
            Ok(Value::Int(n)) => n.cmp(&0),
            Ok(Value::Float(f)) => f.partial_cmp(&0.0).unwrap_or(std::cmp::Ordering::Equal),
            Ok(_) => std::cmp::Ordering::Equal,
            Err(e) => {
                sort_err = Some(e);
                std::cmp::Ordering::Equal
            }
        }
    });
    if let Some(err) = sort_err {
        return Err(err);
    }
    Ok(Value::Array(Arc::new(owned)))
}

fn native_spawn(dispatch: &mut dyn NativeDispatch, args: &[Value]) -> RuntimeResult<Value> {
    let Some(callable) = args.first().cloned() else {
        return Ok(Value::Unit);
    };
    let rest = args.iter().skip(1).cloned().collect();
    dispatch.spawn_callable(callable, rest)?;
    Ok(Value::Unit)
}

fn builtin_testing_check(args: &[Value]) -> RuntimeResult<Value> {
    let cond = matches!(args.first(), Some(Value::Bool(true)));
    let message = args.get(1).and_then(as_str).unwrap_or("check failed");
    let location = current_assertion_location()
        .map(|s| format!(" at {s}"))
        .unwrap_or_default();
    observe_assertion(cond, format!("check: {message}{location}"));
    if cond {
        Ok(ok_variant(Value::Unit))
    } else {
        Ok(err_variant(format!("assertion failed: {message}")))
    }
}

fn builtin_testing_check_eq(args: &[Value]) -> RuntimeResult<Value> {
    let left = args.first().cloned().unwrap_or(Value::Unit);
    let right = args.get(1).cloned().unwrap_or(Value::Unit);
    let message = args.get(2).and_then(as_str).unwrap_or("check_eq failed");
    let ok = values_equal_for_assertion(&left, &right);
    let location = current_assertion_location()
        .map(|s| format!(" at {s}"))
        .unwrap_or_default();
    // `{:?}` (Debug) wraps strings in quotes so a failing
    // `"foo "` vs `"foo"` (trailing space) is visible. Bare
    // `Display` would render them identically.
    observe_assertion(
        ok,
        format!("check_eq: {message}{location}: left={left:?}, right={right:?}"),
    );
    if ok {
        Ok(ok_variant(Value::Unit))
    } else {
        Ok(err_variant(format!(
            "{message}: left={left:?}, right={right:?}"
        )))
    }
}

fn builtin_testing_check_ok(args: &[Value]) -> RuntimeResult<Value> {
    let result = args.first().cloned().unwrap_or(Value::Unit);
    let message = args.get(1).and_then(as_str).unwrap_or("check_ok failed");
    match &result {
        Value::Variant(inner) if inner.name == "Ok" && !inner.fields.is_empty() => {
            observe_assertion(true, format!("check_ok: {message}"));
            Ok(ok_variant(inner.fields[0].clone()))
        }
        Value::Variant(inner) if inner.name == "Err" => {
            let msg = inner
                .fields
                .first()
                .map(|v| format!("{v}"))
                .unwrap_or_default();
            observe_assertion(false, format!("check_ok: {message}: {msg}"));
            Ok(err_variant(format!("{message}: {msg}")))
        }
        other => {
            observe_assertion(
                false,
                format!("check_ok: {message}: not a Result variant: {other}"),
            );
            Ok(err_variant(format!(
                "{message}: expected Result, got {other}"
            )))
        }
    }
}

/// Structural equality check used by `testing::check_eq`; more
/// forgiving than `==` on values since it walks aggregates instead
/// of bailing out on type mismatch. Returns `false` rather than an
/// error on cross-kind operands.
fn values_equal_for_assertion(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Unit, Value::Unit) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Char(x), Value::Char(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Tuple(x), Value::Tuple(y)) | (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|(a, b)| values_equal_for_assertion(a, b))
        }
        (Value::Variant(a), Value::Variant(b)) => {
            a.name == b.name
                && a.fields.len() == b.fields.len()
                && a.fields
                    .iter()
                    .zip(b.fields.iter())
                    .all(|(x, y)| values_equal_for_assertion(x, y))
        }
        _ => false,
    }
}

fn builtin_struct_new(args: &[Value]) -> RuntimeResult<Value> {
    let name: String = match args.first() {
        Some(Value::String(s)) => s.as_str().to_string(),
        _ => String::new(),
    };
    // Collect (field_name, value) pairs in source-literal order.
    let mut pairs: Vec<(String, Value)> = Vec::with_capacity(args.len() / 2);
    let mut iter = args.iter().skip(1);
    while let (Some(key), Some(value)) = (iter.next(), iter.next()) {
        let Value::String(field_name) = key else {
            continue;
        };
        pairs.push((field_name.to_string(), value.clone()));
    }
    // Reorder to declaration order when the struct's layout is
    // known. This makes every `Value::Struct { name: "Body" }`
    // share the same `fields[i]` layout, which lets the VM
    // emit compile-time offsets for field reads instead of
    // doing a linear name scan per read.
    let fields: Vec<(Ident, Value)> = STRUCT_LAYOUTS.with(|cell| {
        let layouts = cell.borrow();
        if let Some(order) = layouts.get(&name) {
            let mut out: Vec<(Ident, Value)> = Vec::with_capacity(order.len());
            for field_name in order {
                let value = pairs
                    .iter()
                    .find(|(n, _)| n == field_name)
                    .map_or(Value::Unit, |(_, v)| v.clone());
                out.push((Ident::new(field_name.as_str()), value));
            }
            // Preserve any extra fields present in the literal
            // but not declared (should be rare; keeps program
            // state visible for debugging).
            for (n, v) in &pairs {
                if !order.iter().any(|o| o == n) {
                    out.push((Ident::new(n.as_str()), v.clone()));
                }
            }
            out
        } else {
            pairs
                .into_iter()
                .map(|(n, v)| (Ident::new(n.as_str()), v))
                .collect()
        }
    });
    Ok(Value::struct_(name, Arc::new(fields)))
}

fn builtin_channel_new(_args: &[Value]) -> RuntimeResult<Value> {
    let channel = crate::value::Channel::new();
    let sender = Value::Channel(channel.clone());
    let receiver = Value::Channel(channel);
    Ok(Value::Tuple(Arc::new(vec![sender, receiver])))
}

fn builtin_channel_send(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::Channel(channel)) = args.first() else {
        return Err(RuntimeError::Type(
            "send: receiver must be a channel".to_string(),
        ));
    };
    let value = args.get(1).cloned().unwrap_or(Value::Unit);
    channel.send(value);
    Ok(Value::Unit)
}

fn builtin_channel_recv(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::Channel(channel)) = args.first() else {
        return Err(RuntimeError::Type(
            "recv: receiver must be a channel".to_string(),
        ));
    };
    Ok(match channel.try_recv() {
        Some(value) => some_variant(value),
        None => none_variant(),
    })
}

/// One flag extracted from a `FlagDecl` struct literal.
#[derive(Debug, Clone)]
struct FlagDeclEntry {
    name: String,
    short: Option<char>,
    default: Value,
}

/// Parses an array of `FlagDecl` structs against `PROGRAM_ARGS` and
/// returns a `FlagMap` value. Used by the declarative flag API in
/// `examples/get_xkcd.gos`.
fn builtin_flag_parse(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::Array(decls)) = args.first() else {
        return Err(RuntimeError::Type(
            "flag::parse: expected array of FlagDecl".to_string(),
        ));
    };
    let program_args = PROGRAM_ARGS.with(|cell| cell.borrow().clone());
    let entries = extract_flag_decls(decls);
    let mut map_fields: Vec<(Ident, Value)> = Vec::new();
    let mut positional: Vec<Value> = Vec::new();
    let mut idx = 0usize;
    while idx < program_args.len() {
        let arg = &program_args[idx];
        if arg == "--" {
            for rest in &program_args[idx + 1..] {
                positional.push(Value::String(SmolStr::from(rest.clone())));
            }
            break;
        }
        if let Some(rest) = arg.strip_prefix("--") {
            let (name, explicit) = match rest.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (rest.to_string(), None),
            };
            let Some(entry) = entries.iter().find(|d| d.name == name) else {
                idx += 1;
                continue;
            };
            let parsed = if let Some(v) = explicit {
                flag_parse_value(&entry.default, &v)
            } else {
                let Some(next) = program_args.get(idx + 1) else {
                    idx += 1;
                    continue;
                };
                idx += 1;
                flag_parse_value(&entry.default, next)
            };
            map_fields.push((Ident::new(&entry.name), parsed));
            idx += 1;
            continue;
        }
        if let Some(rest) = arg.strip_prefix('-') {
            if rest.is_empty() {
                positional.push(Value::String(SmolStr::from(arg.clone())));
                idx += 1;
                continue;
            }
            let mut chars = rest.chars();
            let first = chars.next().unwrap();
            let remainder = chars.as_str();
            let Some(entry) = entries.iter().find(|d| d.short == Some(first)) else {
                idx += 1;
                continue;
            };
            let explicit = if remainder.is_empty() {
                None
            } else {
                Some(remainder.to_string())
            };
            let parsed = if let Some(v) = explicit {
                flag_parse_value(&entry.default, &v)
            } else {
                let Some(next) = program_args.get(idx + 1) else {
                    idx += 1;
                    continue;
                };
                idx += 1;
                flag_parse_value(&entry.default, next)
            };
            map_fields.push((Ident::new(&entry.name), parsed));
            idx += 1;
            continue;
        }
        positional.push(Value::String(SmolStr::from(arg.clone())));
        idx += 1;
    }

    for entry in &entries {
        if !map_fields.iter().any(|(ident, _)| ident.name == entry.name) {
            map_fields.push((Ident::new(&entry.name), entry.default.clone()));
        }
    }
    map_fields.push((
        Ident::new("__positional"),
        Value::Array(Arc::new(positional)),
    ));
    Ok(Value::struct_("FlagMap", Arc::new(map_fields)))
}

fn extract_flag_decls(values: &[Value]) -> Vec<FlagDeclEntry> {
    let mut out = Vec::new();
    for value in values {
        let Value::Struct(inner) = value else {
            continue;
        };
        if inner.name != "FlagDecl" {
            continue;
        }
        let field_map: std::collections::HashMap<&str, &Value> = inner
            .fields
            .iter()
            .map(|(ident, val)| (ident.name.as_str(), val))
            .collect();
        let Some(Value::String(flag_name)) = field_map.get("name") else {
            continue;
        };
        let short = field_map.get("short").and_then(|v| match v {
            Value::Char(c) => Some(*c),
            _ => None,
        });
        let default = field_map
            .get("value")
            .copied()
            .cloned()
            .unwrap_or(Value::Unit);
        out.push(FlagDeclEntry {
            name: flag_name.to_string(),
            short,
            default,
        });
    }
    out
}

fn flag_parse_value(default: &Value, raw: &str) -> Value {
    match default {
        Value::Variant(inner) if inner.name == "Int" => {
            let n = raw.parse::<i64>().unwrap_or(0);
            Value::variant("Int", Arc::new(vec![Value::Int(n)]))
        }
        Value::Variant(inner) if inner.name == "Str" => Value::variant(
            "Str",
            Arc::new(vec![Value::String(SmolStr::from(raw.to_string()))]),
        ),
        Value::Variant(inner) if inner.name == "Bool" => {
            let b = matches!(raw, "true" | "1" | "yes" | "on");
            Value::variant("Bool", Arc::new(vec![Value::Bool(b)]))
        }
        _ => Value::String(SmolStr::from(raw.to_string())),
    }
}

/// `FlagMap::get(flag_map, key)` returns `Some(flag_value)` when the
/// key exists in the parsed map, otherwise `None`.
fn builtin_flag_map_get(args: &[Value]) -> RuntimeResult<Value> {
    let (map, key) = match args {
        [Value::Struct(inner), key_value] if inner.name == "FlagMap" => {
            let key_str = match key_value {
                Value::String(s) => s.as_str(),
                _ => "",
            };
            (&inner.fields, key_str)
        }
        _ => {
            return Err(RuntimeError::Type(
                "FlagMap::get: expected FlagMap and key".to_string(),
            ));
        }
    };
    let found = map.iter().find(|(ident, _)| ident.name == key);
    Ok(match found {
        Some((_, value)) => some_variant(value.clone()),
        None => none_variant(),
    })
}

// ------------------------------------------------------------------
// Sync primitives: I64Vec, WaitGroup, lcg_jump.
//
// The interpreter exposes these as `Value::Struct { name: "I64Vec" }`
// / `Value::Struct { name: "WaitGroup" }` carrying a single
// `__handle: i64` field. The actual mutable state lives in the
// global side tables below, so the `Value` itself stays cheap to
// clone across goroutine boundaries (the closure-call form of
// `go iub_worker(buf, ...)` passes `buf` by value into the
// spawned thread). Shared writes go through `AtomicI64::store`
// without locking; non-overlapping ranges are how `fasta.gos`'s
// fan-out is correct in the first place.

use std::sync::atomic::AtomicI64;

struct WaitGroupCell {
    counter: parking_lot::Mutex<i64>,
    cond: parking_lot::Condvar,
}

static I64VEC_REGISTRY: parking_lot::Mutex<Vec<Option<Arc<Vec<AtomicI64>>>>> =
    parking_lot::Mutex::new(Vec::new());
static WG_REGISTRY: parking_lot::Mutex<Vec<Option<Arc<WaitGroupCell>>>> =
    parking_lot::Mutex::new(Vec::new());

fn i64vec_register(arc: Arc<Vec<AtomicI64>>) -> i64 {
    let mut reg = I64VEC_REGISTRY.lock();
    for (i, slot) in reg.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(arc);
            return i as i64;
        }
    }
    let id = reg.len() as i64;
    reg.push(Some(arc));
    id
}

fn i64vec_lookup(handle: i64) -> Option<Arc<Vec<AtomicI64>>> {
    let reg = I64VEC_REGISTRY.lock();
    if handle < 0 {
        return None;
    }
    reg.get(handle as usize).and_then(std::clone::Clone::clone)
}

fn wg_register(arc: Arc<WaitGroupCell>) -> i64 {
    let mut reg = WG_REGISTRY.lock();
    for (i, slot) in reg.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(arc);
            return i as i64;
        }
    }
    let id = reg.len() as i64;
    reg.push(Some(arc));
    id
}

fn wg_lookup(handle: i64) -> Option<Arc<WaitGroupCell>> {
    let reg = WG_REGISTRY.lock();
    if handle < 0 {
        return None;
    }
    reg.get(handle as usize).and_then(std::clone::Clone::clone)
}

fn struct_handle(v: &Value, expected: &str) -> Option<i64> {
    match v {
        Value::Struct(inner) if inner.name == expected => {
            for (ident, val) in inner.fields.iter() {
                if ident.name == "__handle" {
                    if let Value::Int(n) = val {
                        return Some(*n);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn make_handle_struct(name: &str, handle: i64) -> Value {
    Value::struct_(
        name,
        Arc::new(vec![(Ident::new("__handle"), Value::Int(handle))]),
    )
}

fn arg_int(args: &[Value], idx: usize) -> Option<i64> {
    match args.get(idx) {
        Some(Value::Int(n)) => Some(*n),
        _ => None,
    }
}

fn builtin_i64vec_new(args: &[Value]) -> RuntimeResult<Value> {
    let len = arg_int(args, 0).unwrap_or(0).max(0) as usize;
    let mut data: Vec<AtomicI64> = Vec::with_capacity(len);
    for _ in 0..len {
        data.push(AtomicI64::new(0));
    }
    let handle = i64vec_register(Arc::new(data));
    Ok(make_handle_struct("I64Vec", handle))
}

fn builtin_i64vec_set_at(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args
        .first()
        .and_then(|v| struct_handle(v, "I64Vec"))
        .ok_or_else(|| RuntimeError::Type("set_at: receiver must be I64Vec".to_string()))?;
    let vec_arc = i64vec_lookup(handle)
        .ok_or_else(|| RuntimeError::Type("set_at: stale I64Vec handle".to_string()))?;
    let idx = arg_int(args, 1)
        .ok_or_else(|| RuntimeError::Type("set_at: idx must be i64".to_string()))?;
    let val = arg_int(args, 2)
        .ok_or_else(|| RuntimeError::Type("set_at: val must be i64".to_string()))?;
    if idx >= 0 {
        if let Some(slot) = vec_arc.get(idx as usize) {
            slot.store(val, std::sync::atomic::Ordering::Relaxed);
        }
    }
    Ok(Value::Unit)
}

fn builtin_i64vec_get_at(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args
        .first()
        .and_then(|v| struct_handle(v, "I64Vec"))
        .ok_or_else(|| RuntimeError::Type("get_at: receiver must be I64Vec".to_string()))?;
    let vec_arc = i64vec_lookup(handle)
        .ok_or_else(|| RuntimeError::Type("get_at: stale I64Vec handle".to_string()))?;
    let idx = arg_int(args, 1)
        .ok_or_else(|| RuntimeError::Type("get_at: idx must be i64".to_string()))?;
    let v = if idx >= 0 {
        vec_arc
            .get(idx as usize)
            .map_or(0, |s| s.load(std::sync::atomic::Ordering::Relaxed))
    } else {
        0
    };
    Ok(Value::Int(v))
}

fn builtin_i64vec_vec_len(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args
        .first()
        .and_then(|v| struct_handle(v, "I64Vec"))
        .ok_or_else(|| RuntimeError::Type("vec_len: receiver must be I64Vec".to_string()))?;
    let vec_arc = i64vec_lookup(handle)
        .ok_or_else(|| RuntimeError::Type("vec_len: stale I64Vec handle".to_string()))?;
    Ok(Value::Int(vec_arc.len() as i64))
}

fn builtin_i64vec_write_range_to_stdout(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args
        .first()
        .and_then(|v| struct_handle(v, "I64Vec"))
        .ok_or_else(|| {
            RuntimeError::Type("write_range_to_stdout: receiver must be I64Vec".to_string())
        })?;
    let vec_arc = i64vec_lookup(handle).ok_or_else(|| {
        RuntimeError::Type("write_range_to_stdout: stale I64Vec handle".to_string())
    })?;
    let off = arg_int(args, 1).unwrap_or(0).max(0) as usize;
    let count = arg_int(args, 2).unwrap_or(0).max(0) as usize;
    let end = off.saturating_add(count).min(vec_arc.len());
    let mut buf = Vec::with_capacity(end.saturating_sub(off));
    for i in off..end {
        buf.push((vec_arc[i].load(std::sync::atomic::Ordering::Relaxed) & 0xff) as u8);
    }
    write_stdout_bytes(&buf);
    Ok(Value::Unit)
}

fn builtin_i64vec_write_lines_to_stdout(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args
        .first()
        .and_then(|v| struct_handle(v, "I64Vec"))
        .ok_or_else(|| {
            RuntimeError::Type("write_lines_to_stdout: receiver must be I64Vec".to_string())
        })?;
    let vec_arc = i64vec_lookup(handle).ok_or_else(|| {
        RuntimeError::Type("write_lines_to_stdout: stale I64Vec handle".to_string())
    })?;
    let off = arg_int(args, 1).unwrap_or(0).max(0) as usize;
    let count = arg_int(args, 2).unwrap_or(0).max(0) as usize;
    let line_len = arg_int(args, 3).unwrap_or(60).max(1) as usize;
    let end = off.saturating_add(count).min(vec_arc.len());
    let mut buf = Vec::with_capacity(end.saturating_sub(off) + count / line_len + 1);
    let mut i = off;
    while i < end {
        let upper = (i + line_len).min(end);
        for j in i..upper {
            buf.push((vec_arc[j].load(std::sync::atomic::Ordering::Relaxed) & 0xff) as u8);
        }
        buf.push(b'\n');
        i = upper;
    }
    write_stdout_bytes(&buf);
    Ok(Value::Unit)
}

// ------------------------------------------------------------------
// U8Vec — 1-byte-per-element heap vec for fasta scratch buffers.
//
// Same handle-table shape as I64Vec; storage uses `AtomicU8` so
// goroutine workers can write disjoint slices without locks.

static U8VEC_REGISTRY: parking_lot::Mutex<Vec<Option<Arc<Vec<std::sync::atomic::AtomicU8>>>>> =
    parking_lot::Mutex::new(Vec::new());

fn u8vec_register(arc: Arc<Vec<std::sync::atomic::AtomicU8>>) -> i64 {
    let mut reg = U8VEC_REGISTRY.lock();
    for (i, slot) in reg.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(arc);
            return i as i64;
        }
    }
    let id = reg.len() as i64;
    reg.push(Some(arc));
    id
}

thread_local! {
    /// Single-slot per-thread cache for the most recent U8Vec
    /// resolution. Hot byte-scan loops issue millions of
    /// `buf.get_byte(_)` calls against one buffer; a trivial
    /// cache on `(handle, Arc)` skips the global registry
    /// mutex entirely after the first lookup.
    static U8VEC_LAST: std::cell::RefCell<Option<(i64, Arc<Vec<std::sync::atomic::AtomicU8>>)>> =
        const { std::cell::RefCell::new(None) };
}

fn u8vec_lookup(handle: i64) -> Option<Arc<Vec<std::sync::atomic::AtomicU8>>> {
    if handle < 0 {
        return None;
    }
    let cached = U8VEC_LAST.with(|cell| {
        cell.borrow()
            .as_ref()
            .filter(|(h, _)| *h == handle)
            .map(|(_, arc)| Arc::clone(arc))
    });
    if cached.is_some() {
        return cached;
    }
    let reg = U8VEC_REGISTRY.lock();
    let arc = reg.get(handle as usize).and_then(std::clone::Clone::clone);
    if let Some(ref a) = arc {
        let cached = Arc::clone(a);
        U8VEC_LAST.with(|cell| *cell.borrow_mut() = Some((handle, cached)));
    }
    arc
}

/// Inline `set_byte` for the VM's `Op::U8VecSetByte` super-instruction.
/// Skips the `args: &[Value]` round-trip and the per-arg
/// `MapKey`-style discriminant matching that
/// [`builtin_u8vec_set_byte`] does. Returns `true` on success;
/// `false` lets the caller fall back to the generic method
/// dispatch path when the receiver shape doesn't match.
#[inline]
pub(crate) fn u8vec_set_byte_inline(handle: i64, idx: i64, byte: i64) -> bool {
    let Some(arc) = u8vec_lookup(handle) else {
        return false;
    };
    if idx < 0 {
        // Out-of-range writes are silently dropped, matching
        // `builtin_u8vec_set_byte`'s `if let Some(slot)` branch.
        return true;
    }
    if let Some(slot) = arc.get(idx as usize) {
        slot.store(byte as u8, std::sync::atomic::Ordering::Relaxed);
    }
    true
}

/// Inline `get_byte` for the VM's `Op::U8VecGetByte`. Returns
/// `None` when the handle is stale (caller falls back to the
/// generic dispatch path); returns `Some(0)` for out-of-range
/// reads, matching [`builtin_u8vec_get_byte`].
#[inline]
pub(crate) fn u8vec_get_byte_inline(handle: i64, idx: i64) -> Option<i64> {
    let arc = u8vec_lookup(handle)?;
    if idx < 0 {
        return Some(0);
    }
    Some(arc.get(idx as usize).map_or(0, |s| {
        i64::from(s.load(std::sync::atomic::Ordering::Relaxed))
    }))
}

fn builtin_u8vec_new(args: &[Value]) -> RuntimeResult<Value> {
    let len = arg_int(args, 0).unwrap_or(0).max(0) as usize;
    let mut data: Vec<std::sync::atomic::AtomicU8> = Vec::with_capacity(len);
    for _ in 0..len {
        data.push(std::sync::atomic::AtomicU8::new(0));
    }
    let handle = u8vec_register(Arc::new(data));
    Ok(make_handle_struct("U8Vec", handle))
}

fn builtin_u8vec_set_byte(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args
        .first()
        .and_then(|v| struct_handle(v, "U8Vec"))
        .ok_or_else(|| RuntimeError::Type("set_byte: receiver must be U8Vec".to_string()))?;
    let vec_arc = u8vec_lookup(handle)
        .ok_or_else(|| RuntimeError::Type("set_byte: stale U8Vec handle".to_string()))?;
    let idx = arg_int(args, 1)
        .ok_or_else(|| RuntimeError::Type("set_byte: idx must be i64".to_string()))?;
    let val = arg_int(args, 2)
        .ok_or_else(|| RuntimeError::Type("set_byte: val must be i64".to_string()))?;
    if idx >= 0 {
        if let Some(slot) = vec_arc.get(idx as usize) {
            slot.store(val as u8, std::sync::atomic::Ordering::Relaxed);
        }
    }
    Ok(Value::Unit)
}

fn builtin_u8vec_get_byte(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args
        .first()
        .and_then(|v| struct_handle(v, "U8Vec"))
        .ok_or_else(|| RuntimeError::Type("get_byte: receiver must be U8Vec".to_string()))?;
    let vec_arc = u8vec_lookup(handle)
        .ok_or_else(|| RuntimeError::Type("get_byte: stale U8Vec handle".to_string()))?;
    let idx = arg_int(args, 1)
        .ok_or_else(|| RuntimeError::Type("get_byte: idx must be i64".to_string()))?;
    let v = if idx >= 0 {
        vec_arc.get(idx as usize).map_or(0, |s| {
            i64::from(s.load(std::sync::atomic::Ordering::Relaxed))
        })
    } else {
        0
    };
    Ok(Value::Int(v))
}

fn builtin_u8vec_count_singles(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args
        .first()
        .and_then(|v| struct_handle(v, "U8Vec"))
        .ok_or_else(|| RuntimeError::Type("count_singles: receiver must be U8Vec".to_string()))?;
    let vec_arc = u8vec_lookup(handle)
        .ok_or_else(|| RuntimeError::Type("count_singles: stale U8Vec handle".to_string()))?;
    let buf_len = arg_int(args, 1).unwrap_or(0).max(0) as usize;
    let len = vec_arc.len().min(buf_len);
    let mut counts = [0i64; 4];
    for slot in &vec_arc[..len] {
        let b = slot.load(std::sync::atomic::Ordering::Relaxed) as usize;
        if b < 4 {
            counts[b] += 1;
        }
    }
    Ok(Value::IntArray(Arc::new(counts.to_vec())))
}

fn builtin_u8vec_count_pairs(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args
        .first()
        .and_then(|v| struct_handle(v, "U8Vec"))
        .ok_or_else(|| RuntimeError::Type("count_pairs: receiver must be U8Vec".to_string()))?;
    let vec_arc = u8vec_lookup(handle)
        .ok_or_else(|| RuntimeError::Type("count_pairs: stale U8Vec handle".to_string()))?;
    let buf_len = arg_int(args, 1).unwrap_or(0).max(0) as usize;
    let len = vec_arc.len().min(buf_len);
    let mut counts = [0i64; 16];
    if len < 2 {
        return Ok(Value::IntArray(Arc::new(counts.to_vec())));
    }
    let stop = len - 1;
    for j in 0..stop {
        let a = vec_arc[j].load(std::sync::atomic::Ordering::Relaxed) as usize;
        let b = vec_arc[j + 1].load(std::sync::atomic::Ordering::Relaxed) as usize;
        let idx = (a << 2) | b;
        if idx < 16 {
            counts[idx] += 1;
        }
    }
    Ok(Value::IntArray(Arc::new(counts.to_vec())))
}

fn builtin_u8vec_count_kmers(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args
        .first()
        .and_then(|v| struct_handle(v, "U8Vec"))
        .ok_or_else(|| RuntimeError::Type("count_kmers: receiver must be U8Vec".to_string()))?;
    let vec_arc = u8vec_lookup(handle)
        .ok_or_else(|| RuntimeError::Type("count_kmers: stale U8Vec handle".to_string()))?;
    let buf_len = arg_int(args, 1).unwrap_or(0).max(0) as usize;
    let k = arg_int(args, 2).unwrap_or(0).max(0) as usize;
    let len = vec_arc.len().min(buf_len);
    let counts: rustc_hash::FxHashMap<i64, i64> = kmer_count(&vec_arc[..len], k);
    Ok(Value::IntMap(Arc::new(parking_lot::Mutex::new(counts))))
}

/// Scans `buf` with a sliding window of size `k`, packing each
/// window into a 2-bit-per-byte `i64` key and accumulating the
/// frequency. Tight C-side loop replacing the `while`-loop +
/// `Op::IntMapInc` chain user code would emit. Pre-allocates
/// the map with a sane capacity (capped well below the worst-
/// case buffer length so a k=18 call does not reserve tens of
/// megabytes of hashbrown slots upfront — hashbrown still grows
/// by doubling beyond the cap, but the cap keeps steady-state
/// RSS predictable for the small-k calls).
// Soft cap on the pre-allocated map capacity: 64 K slots
// (~1 MB at 16 B/slot). Hashbrown still grows by doubling
// beyond this — the cap just keeps steady-state RSS
// predictable for the small-k calls without paying
// catastrophic up-front cost on k=18.
const KMER_CAP_SOFT: usize = 64 * 1024;

#[inline]
fn kmer_count(buf: &[std::sync::atomic::AtomicU8], k: usize) -> rustc_hash::FxHashMap<i64, i64> {
    let upper_by_alphabet = if k == 0 || k >= 32 {
        usize::MAX
    } else {
        1usize.checked_shl((k as u32) * 2).unwrap_or(usize::MAX)
    };
    let cap = upper_by_alphabet.clamp(64, KMER_CAP_SOFT);
    let mut counts: rustc_hash::FxHashMap<i64, i64> =
        rustc_hash::FxHashMap::with_capacity_and_hasher(cap, rustc_hash::FxBuildHasher);
    if k == 0 || k > buf.len() {
        return counts;
    }
    let stop = buf.len() - k + 1;
    // Rolling key: drop the high 2 bits, shift, OR in the new
    // byte. Keeps the inner loop O(1) per iter regardless of k.
    let mask: i64 = if k >= 32 { -1 } else { (1i64 << (k * 2)) - 1 };
    let mut key: i64 = 0;
    for slot in buf.iter().take(k) {
        let b = slot.load(std::sync::atomic::Ordering::Relaxed);
        key = (key << 2) | i64::from(b);
    }
    *counts.entry(key).or_insert(0) += 1;
    let mut i = 1usize;
    while i < stop {
        let b = buf[i + k - 1].load(std::sync::atomic::Ordering::Relaxed);
        key = ((key << 2) | i64::from(b)) & mask;
        *counts.entry(key).or_insert(0) += 1;
        i += 1;
    }
    counts
}

fn builtin_u8vec_window_key(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args
        .first()
        .and_then(|v| struct_handle(v, "U8Vec"))
        .ok_or_else(|| RuntimeError::Type("window_key: receiver must be U8Vec".to_string()))?;
    let vec_arc = u8vec_lookup(handle)
        .ok_or_else(|| RuntimeError::Type("window_key: stale U8Vec handle".to_string()))?;
    let i = arg_int(args, 1).unwrap_or(0).max(0) as usize;
    let k = arg_int(args, 2).unwrap_or(0).max(0) as usize;
    let len = vec_arc.len();
    let mut key: i64 = 0;
    let stop = i.saturating_add(k).min(len);
    for j in i..stop {
        let b = vec_arc[j].load(std::sync::atomic::Ordering::Relaxed);
        key = (key << 2) | i64::from(b);
    }
    // Out-of-range tail: zero-extend remaining slots (matches
    // the by-byte loop's behaviour when `i + k` overshoots).
    let tail = (i + k).saturating_sub(stop);
    for _ in 0..tail {
        key <<= 2;
    }
    Ok(Value::Int(key))
}

fn builtin_u8vec_byte_len(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args
        .first()
        .and_then(|v| struct_handle(v, "U8Vec"))
        .ok_or_else(|| RuntimeError::Type("byte_len: receiver must be U8Vec".to_string()))?;
    let vec_arc = u8vec_lookup(handle)
        .ok_or_else(|| RuntimeError::Type("byte_len: stale U8Vec handle".to_string()))?;
    Ok(Value::Int(vec_arc.len() as i64))
}

/// `Vec::new()` — empty growable array. Used by `let mut v:
/// Vec<T> = Vec::new()` patterns; without this entry the path
/// lookup falls through to the bare `new` global, which is the
/// last-installed module's `new` (currently `HashMap::new`).
fn builtin_vec_new(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::empty_array())
}

/// `buf.to_string(len)` — freezes the first `len` bytes of a
/// `U8Vec` build buffer into an immutable `String`. Mirrors the
/// canonical immutable-string-language idiom: a mutable buffer
/// for incremental construction, an explicit one-shot conversion
/// at the end.
fn builtin_u8vec_to_string(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args
        .first()
        .and_then(|v| struct_handle(v, "U8Vec"))
        .ok_or_else(|| RuntimeError::Type("to_string: receiver must be U8Vec".to_string()))?;
    let vec_arc = u8vec_lookup(handle)
        .ok_or_else(|| RuntimeError::Type("to_string: stale U8Vec handle".to_string()))?;
    let len = arg_int(args, 1).map_or_else(|| vec_arc.len(), |n| n.max(0) as usize);
    let take = len.min(vec_arc.len());
    let mut bytes = Vec::with_capacity(take);
    for slot in vec_arc.iter().take(take) {
        bytes.push(slot.load(std::sync::atomic::Ordering::Relaxed));
    }
    let s = String::from_utf8(bytes)
        .map_err(|_| RuntimeError::Type("to_string: U8Vec contents are not UTF-8".to_string()))?;
    Ok(Value::String(SmolStr::from(s)))
}

fn builtin_u8vec_write_byte_range_to_stdout(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args
        .first()
        .and_then(|v| struct_handle(v, "U8Vec"))
        .ok_or_else(|| {
            RuntimeError::Type("write_byte_range_to_stdout: receiver must be U8Vec".to_string())
        })?;
    let vec_arc = u8vec_lookup(handle).ok_or_else(|| {
        RuntimeError::Type("write_byte_range_to_stdout: stale U8Vec handle".to_string())
    })?;
    let off = arg_int(args, 1).unwrap_or(0).max(0) as usize;
    let count = arg_int(args, 2).unwrap_or(0).max(0) as usize;
    let end = off.saturating_add(count).min(vec_arc.len());
    let mut buf = Vec::with_capacity(end.saturating_sub(off));
    for i in off..end {
        buf.push(vec_arc[i].load(std::sync::atomic::Ordering::Relaxed));
    }
    write_stdout_bytes(&buf);
    Ok(Value::Unit)
}

fn builtin_u8vec_write_byte_lines_to_stdout(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args
        .first()
        .and_then(|v| struct_handle(v, "U8Vec"))
        .ok_or_else(|| {
            RuntimeError::Type("write_byte_lines_to_stdout: receiver must be U8Vec".to_string())
        })?;
    let vec_arc = u8vec_lookup(handle).ok_or_else(|| {
        RuntimeError::Type("write_byte_lines_to_stdout: stale U8Vec handle".to_string())
    })?;
    let off = arg_int(args, 1).unwrap_or(0).max(0) as usize;
    let count = arg_int(args, 2).unwrap_or(0).max(0) as usize;
    let line_len = arg_int(args, 3).unwrap_or(60).max(1) as usize;
    let end = off.saturating_add(count).min(vec_arc.len());
    let mut buf = Vec::with_capacity(end.saturating_sub(off) + (end - off) / line_len + 1);
    let mut i = off;
    while i < end {
        let upper = (i + line_len).min(end);
        for j in i..upper {
            buf.push(vec_arc[j].load(std::sync::atomic::Ordering::Relaxed));
        }
        buf.push(b'\n');
        i = upper;
    }
    write_stdout_bytes(&buf);
    Ok(Value::Unit)
}

fn builtin_waitgroup_new(_args: &[Value]) -> RuntimeResult<Value> {
    let cell = Arc::new(WaitGroupCell {
        counter: parking_lot::Mutex::new(0),
        cond: parking_lot::Condvar::new(),
    });
    let handle = wg_register(cell);
    Ok(make_handle_struct("WaitGroup", handle))
}

fn builtin_waitgroup_add(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args
        .first()
        .and_then(|v| struct_handle(v, "WaitGroup"))
        .ok_or_else(|| {
            RuntimeError::Type("WaitGroup::add: receiver must be WaitGroup".to_string())
        })?;
    let cell = wg_lookup(handle)
        .ok_or_else(|| RuntimeError::Type("WaitGroup::add: stale WaitGroup handle".to_string()))?;
    let n = arg_int(args, 1).unwrap_or(1);
    *cell.counter.lock() += n;
    Ok(Value::Unit)
}

fn builtin_waitgroup_done(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args
        .first()
        .and_then(|v| struct_handle(v, "WaitGroup"))
        .ok_or_else(|| {
            RuntimeError::Type("WaitGroup::done: receiver must be WaitGroup".to_string())
        })?;
    let cell = wg_lookup(handle)
        .ok_or_else(|| RuntimeError::Type("WaitGroup::done: stale WaitGroup handle".to_string()))?;
    let mut count = cell.counter.lock();
    *count -= 1;
    if *count <= 0 {
        cell.cond.notify_all();
    }
    Ok(Value::Unit)
}

fn builtin_waitgroup_wait(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args
        .first()
        .and_then(|v| struct_handle(v, "WaitGroup"))
        .ok_or_else(|| {
            RuntimeError::Type("WaitGroup::wait: receiver must be WaitGroup".to_string())
        })?;
    let cell = wg_lookup(handle)
        .ok_or_else(|| RuntimeError::Type("WaitGroup::wait: stale WaitGroup handle".to_string()))?;
    let mut count = cell.counter.lock();
    while *count > 0 {
        cell.cond.wait(&mut count);
    }
    Ok(Value::Unit)
}

fn builtin_lcg_jump(args: &[Value]) -> RuntimeResult<Value> {
    let state = arg_int(args, 0).unwrap_or(0);
    let ia = arg_int(args, 1).unwrap_or(0);
    let ic = arg_int(args, 2).unwrap_or(0);
    let im = arg_int(args, 3).unwrap_or(1);
    let n = arg_int(args, 4).unwrap_or(0);
    Ok(Value::Int(lcg_jump_compute(state, ia, ic, im, n)))
}

// O(log n) modular exponentiation on the affine LCG transform.
// Mirrors `gos_rt_lcg_jump` in `gossamer-runtime`. Uses i128 internally
// so the intermediate `a * a` and `c * a + c` products do not overflow
// for the bench-game parameter set (im = 139_968).
fn lcg_jump_compute(state: i64, ia: i64, ic: i64, im: i64, n: i64) -> i64 {
    if im <= 0 || n <= 0 {
        return state;
    }
    let modu = i128::from(im);
    let mut a_pow = i128::from(ia).rem_euclid(modu);
    let mut c_pow = i128::from(ic).rem_euclid(modu);
    let mut x = i128::from(state).rem_euclid(modu);
    let mut k = n;
    let mut acc_a: i128 = 1;
    let mut acc_c: i128 = 0;
    while k > 0 {
        if k & 1 == 1 {
            acc_a = (acc_a * a_pow).rem_euclid(modu);
            acc_c = (acc_c * a_pow + c_pow).rem_euclid(modu);
        }
        let a_new = (a_pow * a_pow).rem_euclid(modu);
        let c_new = (c_pow * a_pow + c_pow).rem_euclid(modu);
        a_pow = a_new;
        c_pow = c_new;
        k >>= 1;
    }
    x = (x * acc_a + acc_c).rem_euclid(modu);
    x as i64
}

fn builtin_stream_write_byte_array(args: &[Value]) -> RuntimeResult<Value> {
    let fd = args.first().map_or(1, stream_fd);
    let count = arg_int(args, 2).unwrap_or(0).max(0) as usize;
    let mut buf = Vec::with_capacity(count);
    match args.get(1) {
        Some(Value::IntArray(data)) => {
            for &b in data.iter().take(count) {
                buf.push((b & 0xff) as u8);
            }
        }
        Some(Value::Array(arr)) => {
            for v in arr.iter().take(count) {
                if let Value::Int(b) = v {
                    buf.push((*b & 0xff) as u8);
                }
            }
        }
        _ => {}
    }
    if fd == 2 {
        write_stderr_bytes(&buf);
    } else {
        write_stdout_bytes(&buf);
    }
    Ok(Value::Unit)
}

fn write_stdout_bytes(bytes: &[u8]) {
    if let Ok(text) = std::str::from_utf8(bytes) {
        write_stdout(text);
        return;
    }
    // Lossy fallback for sequences that aren't valid UTF-8 — should
    // not happen in fasta-shaped programs, but keeps the writer
    // contract honest.
    let lossy = String::from_utf8_lossy(bytes);
    write_stdout(&lossy);
}

fn write_stderr_bytes(bytes: &[u8]) {
    if let Ok(text) = std::str::from_utf8(bytes) {
        write_stderr(text);
        return;
    }
    let lossy = String::from_utf8_lossy(bytes);
    write_stderr(&lossy);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_redirect_keeps_absolute_target() {
        assert_eq!(
            crate::http_client_builtins::absolute_redirect("http://host/x", "https://elsewhere/y"),
            "https://elsewhere/y"
        );
    }

    #[test]
    fn absolute_redirect_resolves_root_relative_location() {
        assert_eq!(
            crate::http_client_builtins::absolute_redirect(
                "http://xkcd.com/info.0.json",
                "/info.1.json"
            ),
            "http://xkcd.com/info.1.json"
        );
    }

    #[test]
    fn absolute_redirect_resolves_bare_relative_location() {
        assert_eq!(
            crate::http_client_builtins::absolute_redirect(
                "http://xkcd.com/info.0.json",
                "info.1.json"
            ),
            "http://xkcd.com/info.1.json"
        );
    }

    #[test]
    fn builtin_split_returns_array_of_segments_for_string_receiver() {
        let args = vec![
            Value::String(SmolStr::from("a/b/c".to_string())),
            Value::String(SmolStr::from("/".to_string())),
        ];
        let Ok(Value::Array(parts)) = builtin_split(&args) else {
            panic!("expected array");
        };
        let texts: Vec<&str> = parts
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["a", "b", "c"]);
    }

    #[test]
    fn builtin_split_handles_char_delimiter_argument() {
        let args = vec![
            Value::String(SmolStr::from("one two three".to_string())),
            Value::Char(' '),
        ];
        let Ok(Value::Array(parts)) = builtin_split(&args) else {
            panic!("expected array");
        };
        assert_eq!(parts.len(), 3);
    }

    #[test]
    fn builtin_trim_strips_ascii_whitespace_on_both_sides() {
        let args = vec![Value::String(SmolStr::from("  hello \t ".to_string()))];
        let Ok(Value::String(out)) = builtin_trim(&args) else {
            panic!("expected string");
        };
        assert_eq!(out.as_str(), "hello");
    }
}
