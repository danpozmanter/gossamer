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
