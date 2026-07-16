#![allow(
    unused_imports,
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::items_after_statements,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::option_if_let_else,
    clippy::match_same_arms,
    clippy::if_not_else,
    clippy::single_match_else,
    clippy::needless_pass_by_value,
    clippy::manual_let_else,
    clippy::redundant_else,
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::map_unwrap_or,
    clippy::struct_excessive_bools,
    clippy::module_name_repetitions,
    clippy::unnecessary_wraps,
    clippy::large_enum_variant,
    clippy::if_same_then_else,
    clippy::single_match,
    clippy::useless_conversion,
    clippy::needless_borrows_for_generic_args,
    clippy::let_and_return,
    unsafe_op_in_unsafe_fn,
    unsafe_code,
    clippy::missing_safety_doc,
    clippy::undocumented_unsafe_blocks,
    clippy::needless_collect,
    clippy::elidable_lifetime_names,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::missing_const_for_fn,
    clippy::needless_range_loop,
    clippy::cognitive_complexity,
    clippy::unused_io_amount,
    clippy::ptr_arg,
    clippy::ptr_as_ptr,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::single_call_fn,
    clippy::unused_self,
    clippy::range_plus_one,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::cast_ptr_alignment,
    clippy::manual_assert,
    clippy::manual_string_new,
    clippy::match_bool,
    clippy::nonminimal_bool,
    clippy::redundant_pattern_matching,
    clippy::useless_let_if_seq
)]
//! Wires up Gossamer-callable builtins for stdlib modules whose
//! Rust-side implementation already exists but had no user-facing
//! exposure. Each `install_*` helper is invoked from
//! `builtins::install` so user code that writes
//! `strings::join`, `strconv::parse_i64`, `net::TcpStream::connect`,
//! `time::Instant::now`, etc. resolves to a real callable.
//!
//! All builtins return a `Result`-shaped variant (`Ok` / `Err`) on
//! fallible operations so callers can chain `?` without wrapping.

use std::cell::RefCell;
use std::collections::HashMap as StdHashMap;
use std::io::Read as IoRead;
use std::io::Write as IoWrite;
use std::sync::Arc;

use gossamer_ast::Ident;
use std::sync::atomic::{AtomicBool as StdAtomicBool, AtomicI64 as StdAtomicI64, Ordering};

use crate::value::SmolStr;

use gossamer_std::bufio as bufio_std;
use gossamer_std::math as math_std;
#[cfg(not(target_arch = "wasm32"))]
use gossamer_std::net as net_std;
use gossamer_std::os as os_std;
use gossamer_std::path as path_std;
use gossamer_std::strconv as strconv_std;
use gossamer_std::strings as strings_std;
use gossamer_std::unicode as unicode_std;
use gossamer_std::utf8 as utf8_std;

use gossamer_std::iter as iter_std;
use gossamer_std::utf16 as utf16_std;

use crate::builtins::{
    BuiltinFnPub, as_str, err_variant, install_module_pub, none_variant, ok_variant, some_variant,
    value_to_int,
};
use crate::value::{MapKey, NativeCall, NativeDispatch, RuntimeResult, Value};

/// Entry point invoked from `builtins::install`.
use super::*;

pub(crate) fn install_fs_extras(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "fs",
        &[
            ("open", builtin_fs_file_open),
            ("create", builtin_fs_file_create),
            ("is_file", builtin_os_is_file),
            ("is_dir", builtin_os_is_dir),
            ("is_symlink", builtin_os_is_symlink),
            ("file_size", builtin_os_file_size),
            ("metadata", builtin_fs_metadata),
            ("copy", builtin_os_copy),
            ("canonicalize", builtin_os_canonicalize),
            ("temp_dir", builtin_fs_temp_dir),
            ("temp_file", builtin_fs_temp_file),
        ],
        globals,
    );
    // Leaf intrinsic for the injected real-struct `Metadata` wrapper
    // (gossamer-parse autoderive): returns the fields as a 6-tuple the
    // wrapper folds into a struct. Same field order as `builtin_fs_metadata`.
    {
        let q = "__gos_fs_metadata_raw";
        globals.push((q, crate::builtins::builtin_pub(q, builtin_fs_metadata_raw)));
    }
    let methods: &[(&str, BuiltinFnPub)] = &[
        ("File::open", builtin_fs_file_open),
        ("File::create", builtin_fs_file_create),
        ("File::read", builtin_fs_file_read),
        ("File::read_to_string", builtin_fs_file_read_to_string),
        ("File::write", builtin_fs_file_write),
        ("File::write_all", builtin_fs_file_write),
        ("File::flush", builtin_fs_file_flush),
        ("File::close", builtin_fs_file_close),
        ("OpenOptions::new", builtin_fs_open_options_new),
        ("OpenOptions::read", builtin_fs_open_options_read),
        ("OpenOptions::write", builtin_fs_open_options_write),
        ("OpenOptions::append", builtin_fs_open_options_append),
        ("OpenOptions::truncate", builtin_fs_open_options_truncate),
        ("OpenOptions::create", builtin_fs_open_options_create),
        (
            "OpenOptions::create_new",
            builtin_fs_open_options_create_new,
        ),
        ("OpenOptions::open", builtin_fs_open_options_open),
    ];
    for (short, call) in methods {
        let qualified: &'static str = Box::leak(format!("fs::{short}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, *call)));
        globals.push((*short, crate::builtins::builtin_pub(short, *call)));
    }
}

#[derive(Default, Clone)]
struct FsOpenOptionsState {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
}

static NEXT_FS_HANDLE: GlobalReg<i64> =
    GlobalReg::new(|| parking_lot::ReentrantMutex::new(RefCell::new(1)));
static FS_FILE_REGISTRY: GlobalReg<StdHashMap<i64, Arc<parking_lot::Mutex<std::fs::File>>>> =
    GlobalReg::new(|| parking_lot::ReentrantMutex::new(RefCell::new(StdHashMap::new())));
static FS_OPEN_OPTIONS_REGISTRY: GlobalReg<
    StdHashMap<i64, Arc<parking_lot::Mutex<FsOpenOptionsState>>>,
> = GlobalReg::new(|| parking_lot::ReentrantMutex::new(RefCell::new(StdHashMap::new())));

fn next_fs_handle() -> i64 {
    NEXT_FS_HANDLE.with(|c| {
        let mut v = c.borrow_mut();
        let id = *v;
        *v += 1;
        id
    })
}

fn insert_file_handle(file: std::fs::File) -> Value {
    let id = next_fs_handle();
    FS_FILE_REGISTRY.with(|r| {
        r.borrow_mut()
            .insert(id, Arc::new(parking_lot::Mutex::new(file)));
    });
    handle_struct("fs::File", id)
}

fn insert_open_options_handle(opts: FsOpenOptionsState) -> Value {
    let id = next_fs_handle();
    FS_OPEN_OPTIONS_REGISTRY.with(|r| {
        r.borrow_mut()
            .insert(id, Arc::new(parking_lot::Mutex::new(opts)));
    });
    handle_struct("fs::OpenOptions", id)
}

fn fetch_file_handle(id: i64) -> Option<Arc<parking_lot::Mutex<std::fs::File>>> {
    FS_FILE_REGISTRY.with(|r| r.borrow().get(&id).cloned())
}

/// Duplicate a handle while holding its registry/object locks only briefly.
/// The duplicate can then move to the scheduler blocking pool without making a
/// second goroutine wait on the language-level file handle for the duration of
/// an OS syscall.
fn clone_file_handle(id: i64) -> Result<std::fs::File, Value> {
    let Some(file) = fetch_file_handle(id) else {
        return Err(err_variant("File: stale handle"));
    };
    file.lock()
        .try_clone()
        .map_err(|e| err_variant(e.to_string()))
}

fn fetch_open_options_handle(id: i64) -> Option<Arc<parking_lot::Mutex<FsOpenOptionsState>>> {
    FS_OPEN_OPTIONS_REGISTRY.with(|r| r.borrow().get(&id).cloned())
}

fn std_open_options(opts: &FsOpenOptionsState) -> std::fs::OpenOptions {
    let mut out = std::fs::OpenOptions::new();
    out.read(opts.read)
        .write(opts.write)
        .append(opts.append)
        .truncate(opts.truncate)
        .create(opts.create)
        .create_new(opts.create_new);
    out
}

pub(crate) fn builtin_fs_file_open(args: &[Value]) -> RuntimeResult<Value> {
    let path = match arg_str_at(args, 0, "File::open", "path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match gossamer_runtime::sched_global::run_blocking("fs-file-open", move || {
        std::fs::File::open(path)
    }) {
        Ok(Ok(file)) => Ok(ok_variant(insert_file_handle(file))),
        Ok(Err(e)) => Ok(err_variant(e.to_string())),
        Err(e) => Ok(err_variant(e)),
    }
}

pub(crate) fn builtin_fs_file_create(args: &[Value]) -> RuntimeResult<Value> {
    let path = match arg_str_at(args, 0, "File::create", "path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match gossamer_runtime::sched_global::run_blocking("fs-file-create", move || {
        std::fs::File::create(path)
    }) {
        Ok(Ok(file)) => Ok(ok_variant(insert_file_handle(file))),
        Ok(Err(e)) => Ok(err_variant(e.to_string())),
        Err(e) => Ok(err_variant(e)),
    }
}

/// Creates a unique directory beneath the system temporary root. The caller
/// owns the returned directory and should remove it with `fs::remove_dir_all`.
pub(crate) fn builtin_fs_temp_dir(args: &[Value]) -> RuntimeResult<Value> {
    let prefix = match arg_str_at(args, 0, "fs::temp_dir", "prefix") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match gossamer_runtime::sched_global::run_blocking("fs-temp-dir", move || {
        gossamer_std::fs::temp_dir(&prefix)
    }) {
        Ok(Ok(path)) => Ok(ok_variant(Value::String(
            path.to_string_lossy().into_owned().into(),
        ))),
        Ok(Err(e)) => Ok(err_variant(e.to_string())),
        Err(e) => Ok(err_variant(e)),
    }
}

/// Creates a unique temporary file and returns its streaming handle plus path.
/// Closing/removing the file remains the caller's responsibility.
pub(crate) fn builtin_fs_temp_file(args: &[Value]) -> RuntimeResult<Value> {
    let prefix = match arg_str_at(args, 0, "fs::temp_file", "prefix") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match gossamer_runtime::sched_global::run_blocking("fs-temp-file", move || {
        gossamer_std::fs::temp_file(&prefix)
    }) {
        Ok(Ok((file, path))) => Ok(ok_variant(Value::Tuple(
            vec![
                insert_file_handle(file),
                Value::String(path.to_string_lossy().into_owned().into()),
            ]
            .into(),
        ))),
        Ok(Err(e)) => Ok(err_variant(e.to_string())),
        Err(e) => Ok(err_variant(e)),
    }
}

pub(crate) fn builtin_fs_open_options_new(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(insert_open_options_handle(FsOpenOptionsState::default()))
}

fn fs_open_options_set(args: &[Value], field: fn(&mut FsOpenOptionsState) -> &mut bool) -> Value {
    let Some(id) = args.first().and_then(handle_id) else {
        return err_variant("OpenOptions: missing handle");
    };
    let enabled = matches!(args.get(1), Some(Value::Bool(true)));
    let Some(opts) = fetch_open_options_handle(id) else {
        return err_variant("OpenOptions: stale handle");
    };
    *field(&mut opts.lock()) = enabled;
    handle_struct("fs::OpenOptions", id)
}

pub(crate) fn builtin_fs_open_options_read(args: &[Value]) -> RuntimeResult<Value> {
    Ok(fs_open_options_set(args, |opts| &mut opts.read))
}

pub(crate) fn builtin_fs_open_options_write(args: &[Value]) -> RuntimeResult<Value> {
    Ok(fs_open_options_set(args, |opts| &mut opts.write))
}

pub(crate) fn builtin_fs_open_options_append(args: &[Value]) -> RuntimeResult<Value> {
    Ok(fs_open_options_set(args, |opts| &mut opts.append))
}

pub(crate) fn builtin_fs_open_options_truncate(args: &[Value]) -> RuntimeResult<Value> {
    Ok(fs_open_options_set(args, |opts| &mut opts.truncate))
}

pub(crate) fn builtin_fs_open_options_create(args: &[Value]) -> RuntimeResult<Value> {
    Ok(fs_open_options_set(args, |opts| &mut opts.create))
}

pub(crate) fn builtin_fs_open_options_create_new(args: &[Value]) -> RuntimeResult<Value> {
    Ok(fs_open_options_set(args, |opts| &mut opts.create_new))
}

pub(crate) fn builtin_fs_open_options_open(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("OpenOptions::open: missing handle"));
    };
    let path = match arg_str_at(args, 1, "OpenOptions::open", "path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    let Some(opts) = fetch_open_options_handle(id) else {
        return Ok(err_variant("OpenOptions::open: stale handle"));
    };
    let open = std_open_options(&opts.lock());
    match gossamer_runtime::sched_global::run_blocking("fs-open-options", move || open.open(path)) {
        Ok(Ok(file)) => Ok(ok_variant(insert_file_handle(file))),
        Ok(Err(e)) => Ok(err_variant(e.to_string())),
        Err(e) => Ok(err_variant(e)),
    }
}

pub(crate) fn builtin_fs_file_read(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("File::read: missing handle"));
    };
    let max = args
        .get(1)
        .and_then(value_to_int)
        .unwrap_or(4096)
        .clamp(1, 1 << 24);
    let mut file = match clone_file_handle(id) {
        Ok(file) => file,
        Err(_) => return Ok(err_variant("File::read: stale handle")),
    };
    match gossamer_runtime::sched_global::run_blocking("fs-file-read", move || {
        let mut buf = vec![0u8; max as usize];
        file.read(&mut buf).map(|n| {
            buf.truncate(n);
            buf
        })
    }) {
        Ok(Ok(buf)) => Ok(ok_variant(Value::Array(Arc::new(
            buf.into_iter().map(|b| Value::Int(i64::from(b))).collect(),
        )))),
        Ok(Err(e)) => Ok(err_variant(e.to_string())),
        Err(e) => Ok(err_variant(e)),
    }
}

pub(crate) fn builtin_fs_file_read_to_string(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("File::read_to_string: missing handle"));
    };
    let mut file = match clone_file_handle(id) {
        Ok(file) => file,
        Err(_) => return Ok(err_variant("File::read_to_string: stale handle")),
    };
    match gossamer_runtime::sched_global::run_blocking("fs-file-read-string", move || {
        let mut out = String::new();
        file.read_to_string(&mut out).map(|_| out)
    }) {
        Ok(Ok(out)) => Ok(ok_variant(Value::String(out.into()))),
        Ok(Err(e)) => Ok(err_variant(e.to_string())),
        Err(e) => Ok(err_variant(e)),
    }
}

pub(crate) fn builtin_fs_file_write(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("File::write: missing handle"));
    };
    let bytes: Vec<u8> = match args.get(1) {
        Some(Value::String(s)) => s.as_bytes().to_vec(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| match v {
                Value::Int(n) => u8::try_from(*n).ok(),
                _ => None,
            })
            .collect(),
        _ => return Ok(err_variant("File::write: expected string or byte array")),
    };
    let mut file = match clone_file_handle(id) {
        Ok(file) => file,
        Err(_) => return Ok(err_variant("File::write: stale handle")),
    };
    let written = bytes.len() as i64;
    match gossamer_runtime::sched_global::run_blocking("fs-file-write", move || {
        file.write_all(&bytes)
    }) {
        Ok(Ok(())) => Ok(ok_variant(Value::Int(written))),
        Ok(Err(e)) => Ok(err_variant(e.to_string())),
        Err(e) => Ok(err_variant(e)),
    }
}

pub(crate) fn builtin_fs_file_flush(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("File::flush: missing handle"));
    };
    let mut file = match clone_file_handle(id) {
        Ok(file) => file,
        Err(_) => return Ok(err_variant("File::flush: stale handle")),
    };
    match gossamer_runtime::sched_global::run_blocking("fs-file-flush", move || file.flush()) {
        Ok(Ok(())) => Ok(ok_variant(Value::Unit)),
        Ok(Err(e)) => Ok(err_variant(e.to_string())),
        Err(e) => Ok(err_variant(e)),
    }
}

pub(crate) fn builtin_fs_file_close(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(handle_id) {
        FS_FILE_REGISTRY.with(|r| {
            r.borrow_mut().remove(&id);
        });
    }
    Ok(Value::Unit)
}

pub(crate) fn builtin_fs_metadata_raw(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.first().and_then(as_str).unwrap_or("");
    match std::fs::metadata(path) {
        Ok(meta) => {
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
            Ok(ok_variant(Value::Tuple(Arc::from(vec![
                Value::Int(i64::try_from(meta.len()).unwrap_or(i64::MAX)),
                Value::Bool(meta.is_file()),
                Value::Bool(meta.is_dir()),
                Value::Bool(meta.file_type().is_symlink()),
                Value::Bool(meta.permissions().readonly()),
                Value::Int(modified),
            ]))))
        }
        Err(e) => Ok(err_variant(format!("metadata: {e}"))),
    }
}

pub(crate) fn builtin_fs_metadata(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.first().and_then(as_str).unwrap_or("");
    match std::fs::metadata(path) {
        Ok(meta) => {
            let fields = vec![
                (
                    "size",
                    Value::Int(i64::try_from(meta.len()).unwrap_or(i64::MAX)),
                ),
                ("is_file", Value::Bool(meta.is_file())),
                ("is_dir", Value::Bool(meta.is_dir())),
                ("is_symlink", Value::Bool(meta.file_type().is_symlink())),
                ("readonly", Value::Bool(meta.permissions().readonly())),
                (
                    "modified_unix_ms",
                    Value::Int(
                        meta.modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
                    ),
                ),
            ];
            Ok(ok_variant(Value::struct_(
                "fs::Metadata",
                Arc::unwrap_or_clone(Arc::new(fields)),
            )))
        }
        Err(e) => Ok(err_variant(format!("metadata: {e}"))),
    }
}

#[cfg(test)]
mod file_handle_tests {
    use super::*;

    fn ok_payload(value: Value) -> Value {
        match value {
            Value::Variant(inner) if inner.name.as_str() == "Ok" => inner
                .fields
                .first()
                .cloned()
                .expect("Ok payload should exist"),
            other => panic!("expected Ok variant, got {other:?}"),
        }
    }

    fn temp_path(name: &str) -> String {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "gossamer-interp-{name}-{}-{}.txt",
            std::process::id(),
            next_fs_handle()
        ));
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn file_handle_create_write_reopen_read_to_string() {
        let path = temp_path("file-handle");
        let created = ok_payload(
            builtin_fs_file_create(&[Value::String(path.clone().into())]).expect("create"),
        );
        ok_payload(
            builtin_fs_file_write(&[created.clone(), Value::String(SmolStr::from("hello file"))])
                .expect("write"),
        );
        ok_payload(builtin_fs_file_flush(std::slice::from_ref(&created)).expect("flush"));
        assert!(matches!(
            builtin_fs_file_close(std::slice::from_ref(&created)).expect("close"),
            Value::Unit
        ));

        let opened =
            ok_payload(builtin_fs_file_open(&[Value::String(path.clone().into())]).expect("open"));
        let read = ok_payload(
            builtin_fs_file_read_to_string(std::slice::from_ref(&opened)).expect("read"),
        );
        assert!(matches!(read, Value::String(s) if s.as_str() == "hello file"));
        let _ = builtin_fs_file_close(&[opened]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn open_options_handle_opens_with_configured_flags() {
        let path = temp_path("open-options");
        let opts = builtin_fs_open_options_new(&[]).expect("new");
        let opts = builtin_fs_open_options_write(&[opts, Value::Bool(true)]).expect("write");
        let opts = builtin_fs_open_options_create(&[opts, Value::Bool(true)]).expect("create");
        let opts = builtin_fs_open_options_truncate(&[opts, Value::Bool(true)]).expect("truncate");
        let file = ok_payload(
            builtin_fs_open_options_open(&[opts, Value::String(path.clone().into())])
                .expect("open"),
        );
        ok_payload(
            builtin_fs_file_write(&[file.clone(), Value::String(SmolStr::from("via opts"))])
                .expect("write"),
        );
        let _ = builtin_fs_file_close(&[file]);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read file"),
            "via opts"
        );
        let _ = std::fs::remove_file(path);
    }
}

// ----------------------------------------------------------------------
// bufio extras
