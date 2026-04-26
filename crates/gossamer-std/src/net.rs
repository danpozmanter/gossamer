//! Runtime support for `std::net`.
//! Wraps `std::net::{TcpListener, TcpStream, UdpSocket}`
//! behind Gossamer-named types so the eventual `.gos`-sourced stdlib
//! can delegate to them. The wrappers are intentionally thin: later
//! phases will replace them with scheduler-aware non-blocking
//! variants that park goroutines on the network poller.

#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::net::{
    SocketAddr, TcpListener as StdTcpListener, TcpStream as StdTcpStream, ToSocketAddrs,
    UdpSocket as StdUdpSocket,
};

use crate::io::IoError;

/// Bound TCP listener.
#[derive(Debug)]
pub struct TcpListener {
    inner: StdTcpListener,
}

impl TcpListener {
    /// Binds the listener to `addr` and returns the handle.
    pub fn bind(addr: &str) -> Result<Self, IoError> {
        let inner = StdTcpListener::bind(addr).map_err(|e| IoError::from_std(e, addr))?;
        Ok(Self { inner })
    }

    /// Returns the bound local address.
    pub fn local_addr(&self) -> Result<SocketAddr, IoError> {
        self.inner
            .local_addr()
            .map_err(|e| IoError::from_std(e, "local_addr"))
    }

    /// Accepts a single incoming connection.
    pub fn accept(&self) -> Result<(TcpStream, SocketAddr), IoError> {
        let (stream, addr) = self
            .inner
            .accept()
            .map_err(|e| IoError::from_std(e, "accept"))?;
        Ok((TcpStream { inner: stream }, addr))
    }
}

/// Connected TCP byte stream.
#[derive(Debug)]
pub struct TcpStream {
    inner: StdTcpStream,
}

impl TcpStream {
    /// Connects to `addr`.
    pub fn connect(addr: &str) -> Result<Self, IoError> {
        let inner = StdTcpStream::connect(addr).map_err(|e| IoError::from_std(e, addr))?;
        Ok(Self { inner })
    }

    /// Reads up to `buf.len()` bytes into `buf`.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        self.inner
            .read(buf)
            .map_err(|e| IoError::from_std(e, "TcpStream::read"))
    }

    /// Writes every byte in `buf`.
    pub fn write_all(&mut self, buf: &[u8]) -> Result<(), IoError> {
        self.inner
            .write_all(buf)
            .map_err(|e| IoError::from_std(e, "TcpStream::write_all"))
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
