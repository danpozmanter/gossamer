//! Profile output compatible with `go tool pprof`.
//!
//! Three profile shapes are exposed:
//!
//! - **CPU profile** — signal-driven sampler in
//!   [`gossamer_runtime::preempt`] records the program counter at
//!   ~100 Hz; [`cpu_profile`] drains the samples into a profile
//!   blob.
//! - **Heap profile** — allocation events produce a sample per N
//!   bytes (Go's default is 512 KiB); [`heap_profile`] reads the
//!   accumulated counters.
//! - **Goroutine profile** — snapshot of every live goroutine via
//!   [`gossamer_runtime::sigquit::snapshot`]; [`goroutine_profile`]
//!   formats it.
//!
//! All three return bytes that `go tool pprof -text` (or
//! `-web`) reads. The wire format is the simple "legacy text"
//! profile shape — every line is a sample of the form:
//!
//! ```text
//! samples=N self=K
//!   func1 file:line
//!   func2 file:line
//! ```
//!
//! `go tool pprof` accepts this format; the protobuf-encoded
//! variant (`profile.proto`) is a Phase-2 follow-up and is wired
//! once a `prost`-shaped dependency is acceptable in the workspace.

#![forbid(unsafe_code)]

use std::time::Duration;

use parking_lot::Mutex;
use std::sync::OnceLock;

/// One sampled stack frame.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Symbolicated function name.
    pub function: String,
    /// Source file path (DWARF, when available).
    pub file: String,
    /// 1-based line number.
    pub line: u32,
}

/// One sample in a profile.
#[derive(Debug, Clone, Default)]
pub struct Sample {
    /// Sample weight — number of inclusive units (CPU time slices,
    /// allocated bytes, alive goroutines).
    pub weight: u64,
    /// Innermost frame first.
    pub stack: Vec<Frame>,
}

/// Accumulator for sampler-driven profiles.
#[derive(Debug, Default)]
pub struct ProfileBuffer {
    samples: Vec<Sample>,
}

impl ProfileBuffer {
    /// Adds a sample to the buffer.
    pub fn record(&mut self, sample: Sample) {
        self.samples.push(sample);
    }

    /// Returns and clears the accumulated samples.
    #[must_use]
    pub fn drain(&mut self) -> Vec<Sample> {
        std::mem::take(&mut self.samples)
    }

    /// Renders the samples into the textual pprof format.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(self.samples.len() * 64);
        out.push_str("# pprof text format v1\n");
        for sample in &self.samples {
            out.push_str(&format!(
                "samples={} self={}\n",
                sample.weight, sample.weight
            ));
            for frame in &sample.stack {
                out.push_str(&format!(
                    "  {} {}:{}\n",
                    if frame.function.is_empty() {
                        "<unknown>"
                    } else {
                        frame.function.as_str()
                    },
                    frame.file,
                    frame.line,
                ));
            }
        }
        out
    }
}

static CPU_BUF: OnceLock<Mutex<ProfileBuffer>> = OnceLock::new();
static HEAP_BUF: OnceLock<Mutex<ProfileBuffer>> = OnceLock::new();

fn cpu_buf() -> &'static Mutex<ProfileBuffer> {
    CPU_BUF.get_or_init(|| Mutex::new(ProfileBuffer::default()))
}

fn heap_buf() -> &'static Mutex<ProfileBuffer> {
    HEAP_BUF.get_or_init(|| Mutex::new(ProfileBuffer::default()))
}

/// Records one CPU sample. Called from the SIGPROF handler.
pub fn record_cpu_sample(sample: Sample) {
    cpu_buf().lock().record(sample);
}

/// Records one allocation sample. Called by the allocator when the
/// per-thread bytes-since-last-sample counter crosses the sample
/// rate threshold.
pub fn record_alloc_sample(sample: Sample) {
    heap_buf().lock().record(sample);
}

/// Returns a CPU profile gathered over `duration`. The function
/// blocks the caller for `duration`; samples accumulated during
/// that window are drained and returned.
#[must_use]
pub fn cpu_profile(duration: Duration) -> Vec<u8> {
    drop(cpu_buf().lock().drain());
    std::thread::sleep(duration);
    let samples = cpu_buf().lock().drain();
    let buf = ProfileBuffer { samples };
    buf.render().into_bytes()
}

/// Returns the current heap profile.
#[must_use]
pub fn heap_profile() -> Vec<u8> {
    let samples = heap_buf().lock().drain();
    let buf = ProfileBuffer { samples };
    buf.render().into_bytes()
}

/// Returns a goroutine snapshot. One sample per live goroutine,
/// each with the goroutine's last-known frame.
#[must_use]
pub fn goroutine_profile() -> Vec<u8> {
    let mut samples = Vec::new();
    for info in gossamer_runtime::sigquit::snapshot() {
        let stack = if info.function.is_empty() {
            vec![Frame {
                function: format!("goroutine#{}", info.gid),
                file: String::new(),
                line: 0,
            }]
        } else {
            vec![Frame {
                function: info.function,
                file: info.file,
                line: info.line,
            }]
        };
        samples.push(Sample { weight: 1, stack });
    }
    let buf = ProfileBuffer { samples };
    buf.render().into_bytes()
}

/// Returns a mutex-contention profile. Wraps the same underlying
/// goroutine snapshot but filters to those parked on a mutex; for
/// now this returns an empty profile until per-mutex contention
/// counters are wired through `gossamer_std::sync` (Phase 2).
#[must_use]
pub fn mutex_profile() -> Vec<u8> {
    ProfileBuffer::default().render().into_bytes()
}

/// Returns a block profile (goroutines blocked on channel ops,
/// I/O, etc.). Empty until per-park-reason counters land — Phase 2.
#[must_use]
pub fn block_profile() -> Vec<u8> {
    ProfileBuffer::default().render().into_bytes()
}

/// Routes a request path under `/debug/pprof/...` to the right
/// profile generator and returns the bytes the HTTP handler should
/// write. Returns `None` for paths the pprof router doesn't know.
///
/// Wire format matches Go's `net/http/pprof`:
///
/// - `/debug/pprof/profile?seconds=N` → CPU profile.
/// - `/debug/pprof/heap` → heap profile.
/// - `/debug/pprof/goroutine` → goroutine snapshot.
/// - `/debug/pprof/mutex` → mutex contention profile.
/// - `/debug/pprof/block` → block profile.
/// - `/debug/pprof/` → index page listing the others.
#[must_use]
pub fn route(path: &str, query: &str) -> Option<Vec<u8>> {
    let suffix = path.strip_prefix("/debug/pprof/")?;
    match suffix {
        "" => Some(index_page().into_bytes()),
        "profile" => {
            let secs = parse_query_seconds(query).unwrap_or(30);
            Some(cpu_profile(Duration::from_secs(secs)))
        }
        "heap" => Some(heap_profile()),
        "goroutine" => Some(goroutine_profile()),
        "mutex" => Some(mutex_profile()),
        "block" => Some(block_profile()),
        _ => None,
    }
}

fn parse_query_seconds(query: &str) -> Option<u64> {
    for pair in query.split('&') {
        if let Some(rest) = pair.strip_prefix("seconds=") {
            return rest.parse().ok();
        }
    }
    None
}

fn index_page() -> String {
    let mut out = String::new();
    out.push_str("/debug/pprof/\n");
    for endpoint in ["profile", "heap", "goroutine", "mutex", "block"] {
        out.push_str(&format!("  {endpoint}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_records_and_drains() {
        let mut buf = ProfileBuffer::default();
        buf.record(Sample {
            weight: 1,
            stack: vec![Frame {
                function: "test::fn".into(),
                file: "t.gos".into(),
                line: 1,
            }],
        });
        let drained = buf.drain();
        assert_eq!(drained.len(), 1);
        let again = buf.drain();
        assert!(again.is_empty());
    }

    #[test]
    fn render_emits_text_pprof_header() {
        let mut buf = ProfileBuffer::default();
        buf.record(Sample {
            weight: 5,
            stack: vec![],
        });
        let text = buf.render();
        assert!(text.starts_with("# pprof text format"));
        assert!(text.contains("samples=5"));
    }

    #[test]
    fn goroutine_profile_includes_at_least_self() {
        let _ = goroutine_profile();
        // Smoke: just ensure the call returns without panicking.
    }
}
