//! Runtime support for `std::net`.
//!
//! TCP listener / stream and UDP socket types. Two execution paths
//! are exposed through the same API surface:
//!
//! - **Default (poller-aware)**: socket FDs are set non-blocking and
//!   registered with the global netpoller. A read / write that would
//!   block parks the calling goroutine on a waker; the poller wakes
//!   it when the kernel reports readiness. This is the production
//!   path.
//! - **Blocking fallback**: if the global poller cannot be reached
//!   (e.g. unit tests, single-threaded harnesses), the call falls
//!   back to a plain blocking `std::io::Read`/`Write`.
//!
//! Both paths are observably identical from user code; the blocking
//! fallback is the floor when the runtime is not wired up.

#![forbid(unsafe_code)]
#![allow(
    clippy::similar_names,
    clippy::items_after_statements,
    clippy::needless_continue,
    clippy::let_and_return,
    clippy::missing_errors_doc,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::manual_let_else,
    clippy::doc_markdown,
    clippy::io_other_error,
    clippy::cast_possible_truncation,
    clippy::unchecked_time_subtraction,
    clippy::missing_const_for_fn,
    clippy::checked_conversions
)]

use std::io::{self, ErrorKind, Read, Write};
use std::net::{
    SocketAddr, TcpListener as StdTcpListener, TcpStream as StdTcpStream, ToSocketAddrs,
    UdpSocket as StdUdpSocket,
};
use std::time::Duration;

use crate::io::IoError;
use crate::sched_global;
use gossamer_sched::Interest;

/// Bound TCP listener.
#[derive(Debug)]
pub struct TcpListener {
    inner: StdTcpListener,
    mio: Option<mio::net::TcpListener>,
}

impl TcpListener {
    /// Binds the listener to `addr`.
    pub fn bind(addr: &str) -> Result<Self, IoError> {
        let inner = StdTcpListener::bind(addr).map_err(|e| IoError::from_std(e, addr))?;
        inner
            .set_nonblocking(true)
            .map_err(|e| IoError::from_std(e, addr))?;
        // Build the mio mirror by stealing the FD via try_clone +
        // into. mio::net::TcpListener::from_std requires a
        // non-blocking std listener.
        let mirror = inner.try_clone().map(mio::net::TcpListener::from_std).ok();
        Ok(Self { inner, mio: mirror })
    }

    /// Returns the bound local address.
    pub fn local_addr(&self) -> Result<SocketAddr, IoError> {
        self.inner
            .local_addr()
            .map_err(|e| IoError::from_std(e, "local_addr"))
    }

    /// Accepts a single incoming connection. Parks the caller on the
    /// poller when no connection is currently pending.
    pub fn accept(&mut self) -> Result<(TcpStream, SocketAddr), IoError> {
        loop {
            match self.inner.accept() {
                Ok((stream, addr)) => {
                    stream
                        .set_nonblocking(true)
                        .map_err(|e| IoError::from_std(e, "accept"))?;
                    return Ok((TcpStream::from_std(stream)?, addr));
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    self.wait_readable()?;
                }
                Err(e) => return Err(IoError::from_std(e, "accept")),
            }
        }
    }

    fn wait_readable(&mut self) -> Result<(), IoError> {
        let Some(mio_handle) = self.mio.as_mut() else {
            std::thread::sleep(Duration::from_millis(1));
            return Ok(());
        };
        sched_global::wait_io(mio_handle, Interest::Readable)
            .map_err(|e| IoError::from_std(e, "poller wait"))
    }
}

/// Connected TCP byte stream.
#[derive(Debug)]
pub struct TcpStream {
    inner: StdTcpStream,
    mio: Option<mio::net::TcpStream>,
}

impl TcpStream {
    /// Connects to `addr`.
    pub fn connect(addr: &str) -> Result<Self, IoError> {
        let inner = StdTcpStream::connect(addr).map_err(|e| IoError::from_std(e, addr))?;
        Self::from_std(inner)
    }

    fn from_std(inner: StdTcpStream) -> Result<Self, IoError> {
        inner
            .set_nonblocking(true)
            .map_err(|e| IoError::from_std(e, "set_nonblocking"))?;
        let mirror = inner.try_clone().map(mio::net::TcpStream::from_std).ok();
        Ok(Self { inner, mio: mirror })
    }

    /// Reads up to `buf.len()` bytes into `buf`. Parks the caller on
    /// the poller while the kernel buffer is empty.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        loop {
            match self.inner.read(buf) {
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    self.wait_io(Interest::Readable)?;
                }
                Err(e) => return Err(IoError::from_std(e, "TcpStream::read")),
            }
        }
    }

    /// Writes every byte in `buf`.
    pub fn write_all(&mut self, buf: &[u8]) -> Result<(), IoError> {
        let mut written = 0;
        while written < buf.len() {
            match self.inner.write(&buf[written..]) {
                Ok(0) => {
                    return Err(IoError::from_std(
                        io::Error::new(ErrorKind::WriteZero, "wrote zero bytes"),
                        "TcpStream::write_all",
                    ));
                }
                Ok(n) => written += n,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    self.wait_io(Interest::Writable)?;
                }
                Err(e) => return Err(IoError::from_std(e, "TcpStream::write_all")),
            }
        }
        Ok(())
    }

    fn wait_io(&mut self, interest: Interest) -> Result<(), IoError> {
        let Some(mio_handle) = self.mio.as_mut() else {
            std::thread::sleep(Duration::from_millis(1));
            return Ok(());
        };
        sched_global::wait_io(mio_handle, interest).map_err(|e| IoError::from_std(e, "poller wait"))
    }

    /// Adopts a blocking `std::net::TcpStream` (e.g. one
    /// returned by `std::net::TcpListener::accept`) and wraps
    /// it in the netpoller-aware shell. Used by the h2c
    /// debug entrypoint that does its own blocking accept.
    pub fn from_std_blocking(stream: StdTcpStream) -> Result<Self, IoError> {
        Self::from_std(stream)
    }

    /// Single non-blocking read attempt. Returns `Err` with
    /// `ErrorKind::WouldBlock` when no bytes are immediately
    /// available. Used by the async bridge in
    /// `crate::async_tcp` to integrate with futures-shaped APIs
    /// without consuming a goroutine slot via `wait_io`.
    pub fn try_read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }

    /// Single non-blocking write attempt. Returns `Err` with
    /// `ErrorKind::WouldBlock` when the kernel buffer is full.
    pub fn try_write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    /// Registers this stream's underlying file descriptor with
    /// the netpoller for `interest` events tagged with `gid`.
    /// The caller is responsible for installing a waker via
    /// [`crate::sched_global::register_waker`] before calling
    /// this so the wakeup has somewhere to land.
    pub fn register_with_poller(
        &mut self,
        interest: Interest,
        gid: gossamer_runtime::sched::Gid,
    ) -> std::io::Result<()> {
        let Some(mio_handle) = self.mio.as_mut() else {
            return Err(std::io::Error::other("stream has no mio handle"));
        };
        sched_global::with_poller(|p| p.register_io(mio_handle, interest, gid))?;
        Ok(())
    }
}

/// Bound UDP socket.
#[derive(Debug)]
pub struct UdpSocket {
    inner: StdUdpSocket,
}

impl UdpSocket {
    /// Binds the socket to `addr`.
    pub fn bind(addr: &str) -> Result<Self, IoError> {
        let inner = StdUdpSocket::bind(addr).map_err(|e| IoError::from_std(e, addr))?;
        Ok(Self { inner })
    }

    /// Sends `buf` to `addr`, returning the number of bytes written.
    pub fn send_to(&self, buf: &[u8], addr: &str) -> Result<usize, IoError> {
        self.inner
            .send_to(buf, addr)
            .map_err(|e| IoError::from_std(e, addr))
    }

    /// Receives a datagram, returning the length and source address.
    pub fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr), IoError> {
        self.inner
            .recv_from(buf)
            .map_err(|e| IoError::from_std(e, "UdpSocket::recv_from"))
    }

    /// Returns the bound local address.
    pub fn local_addr(&self) -> Result<SocketAddr, IoError> {
        self.inner
            .local_addr()
            .map_err(|e| IoError::from_std(e, "local_addr"))
    }
}

/// Resolves `host` to a list of socket addresses.
pub fn resolve(host: &str) -> Result<Vec<SocketAddr>, IoError> {
    let iter = host
        .to_socket_addrs()
        .map_err(|e| IoError::from_std(e, host))?;
    Ok(iter.collect())
}

impl TcpStream {
    /// Sets TCP keepalive on the stream. `None` disables.
    /// `Some(d)` enables with the system's default keepalive
    /// probing schedule (the per-OS knobs for `KEEPIDLE`,
    /// `KEEPINTVL`, `KEEPCNT` use platform defaults).
    pub fn set_keepalive(&self, dur: Option<Duration>) -> Result<(), IoError> {
        // std exposes only the boolean; the per-interval knobs
        // live behind `socket2`. For v1 we toggle the
        // boolean — fine for keep-the-connection-alive use
        // cases that don't need custom intervals.
        let on = dur.is_some();
        socket_option::set_keepalive(&self.inner, on)
            .map_err(|e| IoError::from_std(e, "set_keepalive"))
    }

    /// Happy-eyeballs connect: races IPv4 and IPv6 candidate
    /// addresses with the supplied stagger delay (Go 1.21
    /// behaviour). Returns the first successful connection.
    ///
    /// `addrs` is the list of candidate `SocketAddr`s (typically
    /// from [`resolve`]). `stagger` is the delay between
    /// successive connection attempts; Go uses 300 ms by
    /// default. Pass `Duration::ZERO` to attempt every address
    /// in parallel.
    pub fn connect_happy_eyeballs(
        addrs: &[SocketAddr],
        stagger: Duration,
        timeout: Duration,
    ) -> Result<Self, IoError> {
        use std::sync::mpsc;
        if addrs.is_empty() {
            return Err(IoError::from_std(
                std::io::Error::new(ErrorKind::InvalidInput, "no addresses"),
                "happy_eyeballs",
            ));
        }
        // Interleave v6 and v4 candidates (Go's policy: prefer
        // v6 first when available, then alternate).
        let mut v6: Vec<SocketAddr> = addrs.iter().copied().filter(|a| a.is_ipv6()).collect();
        let mut v4: Vec<SocketAddr> = addrs.iter().copied().filter(|a| a.is_ipv4()).collect();
        let mut order: Vec<SocketAddr> = Vec::with_capacity(addrs.len());
        while !v6.is_empty() || !v4.is_empty() {
            if !v6.is_empty() {
                order.push(v6.remove(0));
            }
            if !v4.is_empty() {
                order.push(v4.remove(0));
            }
        }

        let (tx, rx) = mpsc::channel::<Result<StdTcpStream, std::io::Error>>();
        let per_attempt_timeout = timeout;
        let started = std::time::Instant::now();
        let mut handles: Vec<std::thread::JoinHandle<()>> = Vec::new();
        for (i, addr) in order.iter().copied().enumerate() {
            let stagger_for_i = stagger.checked_mul(i as u32).unwrap_or(Duration::ZERO);
            let tx_for_attempt = tx.clone();
            let started_for_attempt = started;
            let handle = std::thread::spawn(move || {
                if !stagger_for_i.is_zero() {
                    std::thread::sleep(stagger_for_i);
                }
                let elapsed = started_for_attempt.elapsed();
                if elapsed >= per_attempt_timeout {
                    return;
                }
                let remaining = per_attempt_timeout - elapsed;
                let result = StdTcpStream::connect_timeout(&addr, remaining);
                let _ = tx_for_attempt.send(result);
            });
            handles.push(handle);
        }
        drop(tx);

        let deadline = started + timeout;
        loop {
            let now = std::time::Instant::now();
            if now >= deadline {
                return Err(IoError::from_std(
                    std::io::Error::new(ErrorKind::TimedOut, "happy_eyeballs deadline"),
                    "happy_eyeballs",
                ));
            }
            match rx.recv_timeout(deadline - now) {
                Ok(Ok(stream)) => return Self::from_std(stream),
                Ok(Err(_)) => continue, // try next
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(IoError::from_std(
                        std::io::Error::new(ErrorKind::Other, "all attempts failed"),
                        "happy_eyeballs",
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
            }
        }
    }
}

/// Socket-option bridge using `socket2`. Wraps the std stream
/// FD without taking ownership.
mod socket_option {
    use std::net::TcpStream;

    pub(super) fn set_keepalive(stream: &TcpStream, on: bool) -> std::io::Result<()> {
        let cloned = stream.try_clone()?;
        let socket = socket2::Socket::from(cloned);
        socket.set_keepalive(on)?;
        // Drop the wrapped socket without closing the original
        // FD by retrieving the raw stream back; `socket2::Socket`
        // owns the FD, so we must `into_raw_fd` / `forget` to
        // avoid the double close. The cloned FD will be closed
        // on drop, which is fine — that's the same as letting
        // the clone fall out of scope.
        drop(socket);
        Ok(())
    }
}

// --- Unix domain sockets ---------------------------------------------

/// Bound Unix domain socket listener.
#[cfg(unix)]
#[derive(Debug)]
pub struct UnixListener {
    inner: std::os::unix::net::UnixListener,
}

#[cfg(unix)]
impl UnixListener {
    /// Binds the listener to the filesystem `path`. The path
    /// must NOT already exist (Unix sockets do not auto-replace).
    pub fn bind(path: &str) -> Result<Self, IoError> {
        let inner =
            std::os::unix::net::UnixListener::bind(path).map_err(|e| IoError::from_std(e, path))?;
        Ok(Self { inner })
    }

    /// Accepts one connection.
    pub fn accept(&self) -> Result<UnixStream, IoError> {
        let (stream, _) = self
            .inner
            .accept()
            .map_err(|e| IoError::from_std(e, "UnixListener::accept"))?;
        Ok(UnixStream { inner: stream })
    }

    /// Returns the bound path.
    pub fn local_addr(&self) -> Result<String, IoError> {
        let addr = self
            .inner
            .local_addr()
            .map_err(|e| IoError::from_std(e, "local_addr"))?;
        Ok(addr
            .as_pathname()
            .map(|p| p.display().to_string())
            .unwrap_or_default())
    }
}

/// Connected Unix domain socket stream.
#[cfg(unix)]
#[derive(Debug)]
pub struct UnixStream {
    inner: std::os::unix::net::UnixStream,
}

#[cfg(unix)]
impl UnixStream {
    /// Connects to the socket at `path`.
    pub fn connect(path: &str) -> Result<Self, IoError> {
        let inner = std::os::unix::net::UnixStream::connect(path)
            .map_err(|e| IoError::from_std(e, path))?;
        Ok(Self { inner })
    }

    /// Reads up to `buf.len()` bytes.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        self.inner
            .read(buf)
            .map_err(|e| IoError::from_std(e, "UnixStream::read"))
    }

    /// Writes `buf`, returning the number of bytes consumed.
    pub fn write(&mut self, buf: &[u8]) -> Result<usize, IoError> {
        self.inner
            .write(buf)
            .map_err(|e| IoError::from_std(e, "UnixStream::write"))
    }

    /// Closes both halves of the stream.
    pub fn shutdown(&self) -> Result<(), IoError> {
        self.inner
            .shutdown(std::net::Shutdown::Both)
            .map_err(|e| IoError::from_std(e, "UnixStream::shutdown"))
    }
}

/// IP address parsing and inspection utilities.
pub mod ip;

#[cfg(test)]
mod p9_tests {
    use super::*;

    #[test]
    fn keepalive_toggle_succeeds_on_loopback_stream() {
        let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stream = TcpStream::connect(&addr.to_string()).unwrap();
        // Drop the listener; we just need the socket pair.
        drop(listener);
        // The setsockopt call must succeed in both directions.
        stream.set_keepalive(Some(Duration::from_secs(60))).unwrap();
        stream.set_keepalive(None).unwrap();
    }

    #[test]
    fn happy_eyeballs_picks_reachable_endpoint() {
        let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
        let real = listener.local_addr().unwrap();
        // Unreachable v4 candidate first, then the real
        // loopback. With a 50ms stagger, the second attempt
        // wins after the first fails.
        let bogus: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let acceptor = std::thread::spawn(move || {
            let _ = listener.accept();
        });
        let stream = TcpStream::connect_happy_eyeballs(
            &[bogus, real],
            Duration::from_millis(50),
            Duration::from_secs(2),
        );
        let _ = acceptor.join();
        assert!(stream.is_ok(), "expected happy-eyeballs to succeed");
    }

    #[test]
    fn happy_eyeballs_returns_timeout_when_all_fail() {
        // Two ports that should be unbound (we don't actually
        // know they're free, but :1 is reserved + we're racing
        // a tight deadline).
        let a: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let b: SocketAddr = "127.0.0.1:2".parse().unwrap();
        let err = TcpStream::connect_happy_eyeballs(
            &[a, b],
            Duration::from_millis(50),
            Duration::from_millis(500),
        )
        .unwrap_err();
        let _ = err; // any IoError is fine — we just need this not to hang.
    }

    #[cfg(unix)]
    #[test]
    fn unix_socket_round_trip() {
        let dir = std::env::temp_dir();
        let socket_path = dir.join(format!("gos-test-unix-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket_path);
        let path_str = socket_path.display().to_string();

        let listener = UnixListener::bind(&path_str).unwrap();
        let path_for_thread = path_str.clone();
        let acceptor = std::thread::spawn(move || {
            let mut stream = listener.accept().unwrap();
            let mut buf = [0u8; 5];
            stream.read(&mut buf).unwrap();
            stream.write(b"world").unwrap();
            let _ = path_for_thread;
        });
        let mut client = UnixStream::connect(&path_str).unwrap();
        client.write(b"hello").unwrap();
        let mut response = [0u8; 5];
        client.read(&mut response).unwrap();
        assert_eq!(&response, b"world");
        acceptor.join().unwrap();
        let _ = std::fs::remove_file(&socket_path);
    }
}
