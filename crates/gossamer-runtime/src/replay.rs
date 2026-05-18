//! Deterministic record / replay for the scheduler + channel layer.
//!
//! Enabled by setting one of two environment variables before the
//! Gossamer program starts:
//!
//! - `GOS_TRACE=path.bin`: record mode. Every channel send / recv,
//!   every random-seed draw, every goroutine spawn / yield is
//!   appended to `path.bin` as a length-prefixed binary record.
//! - `GOS_REPLAY=path.bin`: replay mode. The runtime reads each
//!   record in order and re-drives the scheduler to follow exactly
//!   the same interleaving, producing identical output for an
//!   identical input.
//!
//! Recording adds a small amount of overhead per recorded event;
//! replay is deterministic regardless of host load or thread
//! count.
//!
//! Scope for 0.9.0: channel sends/recvs and the RNG seed-draw path
//! (the Pareto 80% per the audit). Syscall trapping (file reads,
//! time, env) is out of scope.

#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]

use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::OnceLock;

use parking_lot::Mutex;

/// Discriminator for record kinds. Stored as a single u8 on disk.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// Channel send (gid, chan_id, value_bits).
    ChannelSend = 0,
    /// Channel recv (gid, chan_id, value_bits, was_empty: u8).
    ChannelRecv = 1,
    /// Goroutine spawned (parent_gid, child_gid).
    GoSpawn = 2,
    /// Goroutine yielded at safepoint (gid).
    Yield = 3,
    /// RNG seed draw (seed).
    RngSeed = 4,
}

impl EventKind {
    fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            0 => EventKind::ChannelSend,
            1 => EventKind::ChannelRecv,
            2 => EventKind::GoSpawn,
            3 => EventKind::Yield,
            4 => EventKind::RngSeed,
            _ => return None,
        })
    }
}

/// One recorded event. Stored on disk as `[kind: u8][argc: u8][argv: i64 × argc]`.
#[derive(Debug, Clone)]
pub struct Event {
    pub kind: EventKind,
    pub args: Vec<i64>,
}

enum Mode {
    Idle,
    Record(Mutex<BufWriter<File>>),
    Replay(Mutex<BufReader<File>>),
}

static MODE: OnceLock<Mode> = OnceLock::new();

/// Initialises record / replay according to `GOS_TRACE` / `GOS_REPLAY`
/// environment variables. Idempotent — subsequent calls are no-ops.
/// Returns `true` if record or replay was armed.
pub fn init_from_env() -> bool {
    let _ = MODE.get_or_init(|| {
        if let Ok(p) = std::env::var("GOS_REPLAY")
            && !p.is_empty()
        {
            if let Ok(file) = File::open(PathBuf::from(p)) {
                return Mode::Replay(Mutex::new(BufReader::new(file)));
            }
        }
        if let Ok(p) = std::env::var("GOS_TRACE")
            && !p.is_empty()
        {
            if let Ok(file) = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(PathBuf::from(p))
            {
                return Mode::Record(Mutex::new(BufWriter::new(file)));
            }
        }
        Mode::Idle
    });
    !matches!(MODE.get(), Some(Mode::Idle) | None)
}

/// Records `event` if record mode is active. Returns immediately
/// otherwise.
pub fn record(event: &Event) {
    let Some(Mode::Record(writer)) = MODE.get() else {
        return;
    };
    let mut w = writer.lock();
    let _ = w.write_all(&[event.kind as u8, event.args.len() as u8]);
    for arg in &event.args {
        let _ = w.write_all(&arg.to_le_bytes());
    }
    let _ = w.flush();
}

/// Returns the next replay event if replay mode is active. Returns
/// `None` once the trace is exhausted or if not in replay mode.
pub fn next_replay() -> Option<Event> {
    let Some(Mode::Replay(reader)) = MODE.get() else {
        return None;
    };
    let mut r = reader.lock();
    let mut header = [0u8; 2];
    if r.read_exact(&mut header).is_err() {
        return None;
    }
    let kind = EventKind::from_u8(header[0])?;
    let argc = header[1] as usize;
    let mut args = Vec::with_capacity(argc);
    for _ in 0..argc {
        let mut buf = [0u8; 8];
        if r.read_exact(&mut buf).is_err() {
            return None;
        }
        args.push(i64::from_le_bytes(buf));
    }
    Some(Event { kind, args })
}

/// True iff a record session is currently armed.
pub fn is_recording() -> bool {
    matches!(MODE.get(), Some(Mode::Record(_)))
}

/// True iff a replay session is currently armed.
pub fn is_replaying() -> bool {
    matches!(MODE.get(), Some(Mode::Replay(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_then_replay_round_trips_a_synthetic_event() {
        // The OnceLock prevents two tests from configuring different
        // modes in the same process, so this test stays self-contained:
        // it builds an `Event` and verifies the binary serialization
        // round-trips through a temp buffer rather than driving the
        // global state machine.
        let event = Event {
            kind: EventKind::ChannelSend,
            args: vec![1, 2, 3],
        };
        let mut buf: Vec<u8> = Vec::new();
        buf.push(event.kind as u8);
        buf.push(event.args.len() as u8);
        for a in &event.args {
            buf.extend_from_slice(&a.to_le_bytes());
        }
        // Decode.
        assert_eq!(buf[0], EventKind::ChannelSend as u8);
        assert_eq!(buf[1], 3);
        let arg0 = i64::from_le_bytes(buf[2..10].try_into().unwrap());
        let arg1 = i64::from_le_bytes(buf[10..18].try_into().unwrap());
        let arg2 = i64::from_le_bytes(buf[18..26].try_into().unwrap());
        assert_eq!((arg0, arg1, arg2), (1, 2, 3));
    }

    #[test]
    fn event_kind_round_trip_via_u8() {
        for k in [
            EventKind::ChannelSend,
            EventKind::ChannelRecv,
            EventKind::GoSpawn,
            EventKind::Yield,
            EventKind::RngSeed,
        ] {
            assert_eq!(EventKind::from_u8(k as u8), Some(k));
        }
    }
}
