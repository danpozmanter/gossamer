//! `http::ResponseStream::new()` builtins - a response body a handler
//! writes as it goes.
//!
//! The queue itself is `gossamer_runtime::c_abi::http_stream_writer`'s, so
//! the framing a client sees is the same one the compiled tiers produce.
//! Only the registry differs: the VM drains through its own
//! `StreamResponse` registry, so the reading end is registered there.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

use gossamer_std::http::{StatusCode, StreamResponse};
use parking_lot::Mutex;

use crate::builtins::{BuiltinFnPub, as_str, value_to_int};
use crate::value::{RuntimeResult, Value};

/// The reading end of a response stream: whatever a handler wrote, in
/// order, then EOF once every writer is gone.
struct QueueReader {
    rx: Mutex<Receiver<Vec<u8>>>,
    pending: Vec<u8>,
    consumed: usize,
}

impl std::io::Read for QueueReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.consumed >= self.pending.len() {
            match self.rx.lock().recv() {
                Ok(chunk) => {
                    self.pending = chunk;
                    self.consumed = 0;
                }
                // Every writer is gone: the body is complete.
                Err(_) => return Ok(0),
            }
        }
        let available = &self.pending[self.consumed..];
        let take = available.len().min(out.len());
        out[..take].copy_from_slice(&available[..take]);
        self.consumed += take;
        Ok(take)
    }
}

/// Writers by stream handle. An entry is removed by `close`, which is
/// what ends the body.
static WRITERS: Mutex<Option<std::collections::HashMap<i64, Sender<Vec<u8>>>>> = Mutex::new(None);

fn handle_of(args: &[Value]) -> i64 {
    args.first()
        .and_then(crate::http_client_builtins::response_stream_handle)
        .unwrap_or(-1)
}

/// Hands `bytes` to the stream's reader. Answers how many bytes were
/// queued, or `-1` when the stream is closed - which is also what a
/// client that hung up looks like, so a producer can stop.
fn push(handle: i64, bytes: Vec<u8>) -> i64 {
    let len = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
    let guard = WRITERS.lock();
    match guard.as_ref().and_then(|map| map.get(&handle)) {
        Some(tx) if tx.send(bytes).is_ok() => len,
        _ => -1,
    }
}

/// Registers the `http::ResponseStream` builtins.
pub(crate) fn install_http_response_stream(globals: &mut Vec<(&'static str, Value)>) {
    let methods: &[(&str, BuiltinFnPub)] = &[
        ("new", builtin_new),
        ("write", builtin_write),
        ("write_bytes", builtin_write_bytes),
        ("close", builtin_close),
        ("is_open", builtin_is_open),
    ];
    for &(method, call) in methods {
        for key in [
            Box::leak(format!("ResponseStream::{method}").into_boxed_str()),
            Box::leak(format!("http::ResponseStream::{method}").into_boxed_str()),
        ] {
            globals.push((key, crate::builtins::builtin_pub(key, call)));
        }
    }
}

fn builtin_new(_args: &[Value]) -> RuntimeResult<Value> {
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let reader = QueueReader {
        rx: Mutex::new(rx),
        pending: Vec::new(),
        consumed: 0,
    };
    let boxed: Box<dyn std::io::Read + Send + Sync + 'static> = Box::new(reader);
    let handle = crate::http_client_builtins::stream_register_public(StreamResponse::from_reader(
        StatusCode::OK,
        boxed,
    ));
    WRITERS
        .lock()
        .get_or_insert_with(std::collections::HashMap::new)
        .insert(handle, tx);
    Ok(Value::struct_(
        "ResponseStream",
        Arc::unwrap_or_clone(Arc::new(vec![
            ("__handle", Value::Int(handle)),
            ("status", Value::Int(200)),
            ("content_type", Value::String("".into())),
        ])),
    ))
}

fn builtin_write(args: &[Value]) -> RuntimeResult<Value> {
    let handle = handle_of(args);
    let text = as_str(args.get(1).unwrap_or(&Value::Unit))
        .unwrap_or("")
        .as_bytes()
        .to_vec();
    Ok(Value::Int(push(handle, text)))
}

fn builtin_write_bytes(args: &[Value]) -> RuntimeResult<Value> {
    let handle = handle_of(args);
    let bytes = match args.get(1) {
        Some(Value::ByteVec(b)) => b.as_ref().clone(),
        Some(Value::ByteArray(b)) => b.to_vec(),
        Some(Value::InlineByteArray(b)) => b.as_ref().to_vec(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| value_to_int(v).map(|n| n as u8))
            .collect(),
        _ => Vec::new(),
    };
    Ok(Value::Int(push(handle, bytes)))
}

fn builtin_close(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(map) = WRITERS.lock().as_mut() {
        map.remove(&handle_of(args));
    }
    Ok(Value::Unit)
}

fn builtin_is_open(args: &[Value]) -> RuntimeResult<Value> {
    let handle = handle_of(args);
    let open = WRITERS
        .lock()
        .as_ref()
        .is_some_and(|map| map.contains_key(&handle));
    Ok(Value::Bool(open))
}
