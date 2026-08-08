//! Composable reader / writer handles for `std::io`.
//!
//! Every stream adapter the language exposes - in-memory readers and
//! writers, `limit_reader`, `tee_reader`, `multi_reader`, and `pipe` -
//! is a small integer handle into one process-wide registry. Handles
//! compose by holding the ids of their sources, so a chain such as
//! `tee_reader(limit_reader(src, 8), sink)` is three registry rows and
//! no trait objects, which is what lets the compiled tiers carry the
//! same value as a bare `i64`.
//!
//! The runtime mirrors this module in `gossamer-runtime`'s
//! `c_abi::io_handles`; the two must stay behaviourally identical.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use parking_lot::Mutex;

/// Shared byte buffer behind a `pipe()` pair.
#[derive(Debug, Default)]
pub struct PipeBuffer {
    bytes: Mutex<Vec<u8>>,
    closed: AtomicBool,
}

/// One registry row: what a handle actually is.
#[derive(Debug)]
enum Node {
    /// In-memory reader over a fixed byte buffer.
    Memory { data: Vec<u8>, pos: usize },
    /// In-memory writer collecting everything written to it.
    Buffer(Vec<u8>),
    /// Reader yielding at most `remaining` more bytes from `src`.
    Limit { src: i64, remaining: i64 },
    /// Reader forwarding every byte read from `src` into `sink`.
    Tee { src: i64, sink: i64 },
    /// Reader draining each source in turn.
    Multi { sources: Vec<i64>, index: usize },
    /// One half of a `pipe()` pair.
    Pipe(Arc<PipeBuffer>),
}

static NEXT_ID: AtomicI64 = AtomicI64::new(1);
static REGISTRY: Mutex<Option<HashMap<i64, Node>>> = Mutex::new(None);

fn insert(node: Node) -> i64 {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut guard = REGISTRY.lock();
    guard.get_or_insert_with(HashMap::new).insert(id, node);
    id
}

/// Registers an in-memory reader over `text`; returns its handle.
#[must_use]
pub fn string_reader(text: &str) -> i64 {
    insert(Node::Memory {
        data: text.as_bytes().to_vec(),
        pos: 0,
    })
}

/// Registers an in-memory writer; returns its handle.
#[must_use]
pub fn buffer_writer() -> i64 {
    insert(Node::Buffer(Vec::new()))
}

/// Registers a reader yielding at most `limit` bytes from `src`.
#[must_use]
pub fn limit_reader(src: i64, limit: i64) -> i64 {
    insert(Node::Limit {
        src,
        remaining: limit.max(0),
    })
}

/// Registers a reader that mirrors everything read from `src` into
/// `sink` before returning it.
#[must_use]
pub fn tee_reader(src: i64, sink: i64) -> i64 {
    insert(Node::Tee { src, sink })
}

/// Registers a reader that drains each source in turn.
#[must_use]
pub fn multi_reader(sources: Vec<i64>) -> i64 {
    insert(Node::Multi { sources, index: 0 })
}

/// Registers a connected reader / writer pair sharing one buffer.
#[must_use]
pub fn pipe() -> (i64, i64) {
    let shared = Arc::new(PipeBuffer::default());
    let reader = insert(Node::Pipe(Arc::clone(&shared)));
    let writer = insert(Node::Pipe(shared));
    (reader, writer)
}

/// What a handle needs from the registry to serve one read, taken as a
/// snapshot so nested reads never hold the registry lock.
enum Step {
    Direct(Vec<u8>),
    Limit { src: i64, take: i64 },
    Tee { src: i64, sink: i64 },
    Multi { source: i64, index: usize },
    Exhausted,
}

fn step(id: i64, max: usize) -> Step {
    let mut guard = REGISTRY.lock();
    let Some(table) = guard.as_mut() else {
        return Step::Exhausted;
    };
    match table.get_mut(&id) {
        Some(Node::Memory { data, pos }) => {
            let end = (*pos + max).min(data.len());
            let out = data[*pos..end].to_vec();
            *pos = end;
            Step::Direct(out)
        }
        Some(Node::Pipe(shared)) => {
            let shared = Arc::clone(shared);
            drop(guard);
            let mut bytes = shared.bytes.lock();
            let take = max.min(bytes.len());
            Step::Direct(bytes.drain(..take).collect())
        }
        Some(Node::Limit { src, remaining }) => {
            let take = (*remaining).min(max as i64);
            Step::Limit { src: *src, take }
        }
        Some(Node::Tee { src, sink }) => Step::Tee {
            src: *src,
            sink: *sink,
        },
        Some(Node::Multi { sources, index }) => match sources.get(*index) {
            Some(source) => Step::Multi {
                source: *source,
                index: *index,
            },
            None => Step::Exhausted,
        },
        Some(Node::Buffer(_)) | None => Step::Exhausted,
    }
}

fn consume_limit(id: i64, n: i64) {
    let mut guard = REGISTRY.lock();
    if let Some(Node::Limit { remaining, .. }) = guard.as_mut().and_then(|t| t.get_mut(&id)) {
        *remaining = (*remaining - n).max(0);
    }
}

fn advance_multi(id: i64, from: usize) {
    let mut guard = REGISTRY.lock();
    if let Some(Node::Multi { index, .. }) = guard.as_mut().and_then(|t| t.get_mut(&id))
        && *index == from
    {
        *index += 1;
    }
}

/// Reads up to `max` bytes from the reader handle `id`. An empty
/// result means end of stream.
#[must_use]
pub fn read(id: i64, max: usize) -> Vec<u8> {
    if max == 0 {
        return Vec::new();
    }
    match step(id, max) {
        Step::Direct(bytes) => bytes,
        Step::Limit { src, take } => {
            if take <= 0 {
                return Vec::new();
            }
            let out = read(src, take as usize);
            consume_limit(id, out.len() as i64);
            out
        }
        Step::Tee { src, sink } => {
            let out = read(src, max);
            if !out.is_empty() {
                write(sink, &out);
            }
            out
        }
        Step::Multi { source, index } => {
            let out = read(source, max);
            if out.is_empty() {
                advance_multi(id, index);
                return read(id, max);
            }
            out
        }
        Step::Exhausted => Vec::new(),
    }
}

/// Appends `bytes` to the writer handle `id`; returns the number of
/// bytes accepted (zero for an unknown or closed handle).
pub fn write(id: i64, bytes: &[u8]) -> usize {
    let mut guard = REGISTRY.lock();
    let Some(table) = guard.as_mut() else {
        return 0;
    };
    match table.get_mut(&id) {
        Some(Node::Buffer(buf)) => {
            buf.extend_from_slice(bytes);
            bytes.len()
        }
        Some(Node::Pipe(shared)) => {
            let shared = Arc::clone(shared);
            drop(guard);
            if shared.closed.load(Ordering::Acquire) {
                return 0;
            }
            shared.bytes.lock().extend_from_slice(bytes);
            bytes.len()
        }
        _ => 0,
    }
}

/// Copies at most `n` bytes from `src` to `dst`; returns the count
/// actually transferred.
#[must_use]
pub fn copy_n(dst: i64, src: i64, n: i64) -> i64 {
    let mut moved = 0i64;
    while moved < n {
        let want = ((n - moved) as usize).min(8192);
        let chunk = read(src, want);
        if chunk.is_empty() {
            break;
        }
        let _ = write(dst, &chunk);
        moved += chunk.len() as i64;
    }
    moved
}

/// Drains the reader handle `id` to end of stream as UTF-8 text;
/// invalid sequences are replaced.
#[must_use]
pub fn drain(id: i64) -> String {
    let mut out: Vec<u8> = Vec::new();
    loop {
        let chunk = read(id, 8192);
        if chunk.is_empty() {
            break;
        }
        out.extend_from_slice(&chunk);
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Everything written to the writer handle `id`, as UTF-8 text.
#[must_use]
pub fn contents(id: i64) -> String {
    let guard = REGISTRY.lock();
    let bytes = match guard.as_ref().and_then(|t| t.get(&id)) {
        Some(Node::Buffer(buf)) => buf.clone(),
        Some(Node::Pipe(shared)) => shared.bytes.lock().clone(),
        _ => Vec::new(),
    };
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Marks a pipe writer closed so its reader stops accepting writes.
pub fn close_writer(id: i64) {
    let guard = REGISTRY.lock();
    if let Some(Node::Pipe(shared)) = guard.as_ref().and_then(|t| t.get(&id)) {
        shared.closed.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod io_handles_tests {
    use super::*;

    #[test]
    fn limit_reader_stops_at_its_budget() {
        let src = string_reader("abcdefgh");
        let limited = limit_reader(src, 3);
        assert_eq!(drain(limited), "abc");
    }

    #[test]
    fn tee_reader_mirrors_into_the_sink() {
        let src = string_reader("hello");
        let sink = buffer_writer();
        let tee = tee_reader(src, sink);
        assert_eq!(drain(tee), "hello");
        assert_eq!(contents(sink), "hello");
    }

    #[test]
    fn multi_reader_concatenates_sources_in_order() {
        let a = string_reader("one ");
        let b = string_reader("two");
        let multi = multi_reader(vec![a, b]);
        assert_eq!(drain(multi), "one two");
    }

    #[test]
    fn pipe_moves_bytes_from_writer_to_reader() {
        let (reader, writer) = pipe();
        write(writer, b"payload");
        close_writer(writer);
        assert_eq!(drain(reader), "payload");
    }

    #[test]
    fn copy_n_transfers_at_most_the_requested_count() {
        let src = string_reader("abcdef");
        let dst = buffer_writer();
        assert_eq!(copy_n(dst, src, 4), 4);
        assert_eq!(contents(dst), "abcd");
    }
}
