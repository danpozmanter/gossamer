#![allow(
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::needless_range_loop,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::doc_markdown,
    clippy::manual_div_ceil
)]

//! RFC 6455 WebSocket framing engine.
//!
//! Self-contained, dependency-light frame codec shared by every
//! Gossamer execution tier: the bytecode VM (`gossamer-interp`), the
//! Cranelift JIT, and the LLVM AOT runtime (`gossamer-runtime`) all
//! drive the same [`WebSocket`] state machine, so the wire behaviour is
//! identical across tiers. The http-dependent server handshake
//! (`accept`) lives in `gossamer-std` and calls into this crate for the
//! frame layer and the [`compute_accept`] key derivation.
//!
//! Frame format per RFC 6455 §5.2. Server-side does NOT mask frames;
//! client-side masking is required and applied on every outbound frame.
//! Fragmentation, ping/pong, and the graceful close handshake are all
//! handled by [`WebSocket::receive`]. Backpressure is straightforward:
//! `send_*` calls block on the underlying writer.

use std::io::{self, Read, Write};

/// WebSocket message.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// Text frame (UTF-8 payload).
    Text(String),
    /// Binary frame.
    Binary(Vec<u8>),
    /// Ping (with payload echoed by peer in pong).
    Ping(Vec<u8>),
    /// Pong (response to ping).
    Pong(Vec<u8>),
    /// Peer initiated close.
    Close {
        /// Close status code per RFC 6455 §7.4 (e.g. 1000 for normal
        /// close, 1001 for going away).
        code: u16,
        /// Optional human-readable reason.
        reason: String,
    },
}

/// RFC 6455 §5.2 payload-length markers: a 7-bit length field of 126
/// means "the next 2 bytes are the length", 127 means "the next 8 bytes".
const LEN_MARKER_16: u8 = 126;
const LEN_MARKER_64: u8 = 127;

/// RFC 6455 opcodes.
const OP_CONTINUATION: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

/// The RFC 6455 §4.2.2 handshake GUID appended to a client nonce before
/// hashing to form the `Sec-WebSocket-Accept` token.
pub const ACCEPT_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Errors raised by the WebSocket layer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Underlying I/O failure.
    #[error("ws io: {0}")]
    Io(#[from] io::Error),
    /// Peer sent a frame with an unknown opcode.
    #[error("ws protocol: bad opcode {0:#x}")]
    BadOpcode(u8),
    /// Peer sent a payload larger than the configured maximum.
    #[error("ws protocol: payload exceeds {limit} bytes")]
    PayloadTooLarge {
        /// Configured maximum.
        limit: usize,
    },
    /// Peer sent a UTF-8 text frame with invalid bytes.
    #[error("ws protocol: invalid UTF-8 in text frame")]
    InvalidUtf8,
    /// Client did not mask its frames (server-side requires).
    #[error("ws protocol: client frame not masked")]
    UnmaskedClientFrame,
    /// Handshake-level rejection.
    #[error("ws handshake: {0}")]
    Handshake(String),
    /// Peer closed cleanly.
    #[error("ws closed")]
    Closed,
}

/// Computes the `Sec-WebSocket-Accept` token per RFC 6455 §4.2.2:
/// `base64(sha1(key + GUID))`.
#[must_use]
pub fn compute_accept(client_key: &str) -> String {
    let mut combined = String::with_capacity(client_key.len() + ACCEPT_GUID.len());
    combined.push_str(client_key.trim());
    combined.push_str(ACCEPT_GUID);
    let digest = sha1(combined.as_bytes());
    base64_encode(&digest)
}

/// SHA-1 implementation (RFC 3174). Used solely for the WebSocket
/// handshake - not exposed publicly to discourage use elsewhere. SHA-1
/// is broken for collision-resistance but the WebSocket spec mandates it
/// for handshake compatibility.
fn sha1(input: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let bits = (input.len() as u64).wrapping_mul(8);
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bits.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(((bytes.len() + 2) / 3) * 4);
    let mut i = 0;
    while i + 2 < bytes.len() {
        let b1 = bytes[i];
        let b2 = bytes[i + 1];
        let b3 = bytes[i + 2];
        out.push(ALPHABET[(b1 >> 2) as usize] as char);
        out.push(ALPHABET[(((b1 & 0x03) << 4) | (b2 >> 4)) as usize] as char);
        out.push(ALPHABET[(((b2 & 0x0F) << 2) | (b3 >> 6)) as usize] as char);
        out.push(ALPHABET[(b3 & 0x3F) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let b1 = bytes[i];
        out.push(ALPHABET[(b1 >> 2) as usize] as char);
        out.push(ALPHABET[((b1 & 0x03) << 4) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let b1 = bytes[i];
        let b2 = bytes[i + 1];
        out.push(ALPHABET[(b1 >> 2) as usize] as char);
        out.push(ALPHABET[(((b1 & 0x03) << 4) | (b2 >> 4)) as usize] as char);
        out.push(ALPHABET[((b2 & 0x0F) << 2) as usize] as char);
        out.push('=');
    }
    out
}

/// Largest opening-handshake header block accepted before the
/// connection is rejected (slowloris / unbounded-header defense). 16 KiB
/// comfortably holds any well-formed upgrade request or `101` response.
const MAX_HANDSHAKE_BYTES: usize = 16 * 1024;

/// Reads an HTTP head (request or response) one byte at a time until the
/// terminating `\r\n\r\n`. Byte-at-a-time so no WebSocket frame bytes are
/// consumed past the handshake: the stream cursor lands exactly on the
/// first frame.
fn read_handshake_head<R: Read>(stream: &mut R) -> Result<Vec<u8>, Error> {
    let mut buf = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte)?;
        if n == 0 {
            return Err(Error::Handshake("eof during handshake".to_string()));
        }
        buf.push(byte[0]);
        if buf.len() > MAX_HANDSHAKE_BYTES {
            return Err(Error::Handshake(
                "handshake header block too large".to_string(),
            ));
        }
        if buf.ends_with(b"\r\n\r\n") {
            return Ok(buf);
        }
    }
}

/// Case-insensitive lookup of the first `name` header in a raw head
/// (request/status line + headers, up to the blank line).
fn header_value<'a>(head: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    for line in head.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        // The request/status line has no colon - skip it (and any other
        // colon-free line) rather than ending the scan.
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            continue;
        };
        if line[..colon].eq_ignore_ascii_case(name) {
            return Some(line[colon + 1..].trim_ascii());
        }
    }
    None
}

/// Parses the status code out of a `101`-style response head
/// (`b"HTTP/1.1 101 ..."` -> `Some(101)`).
fn response_status(head: &[u8]) -> Option<u16> {
    let line_end = head.windows(2).position(|w| w == b"\r\n")?;
    let line = std::str::from_utf8(&head[..line_end]).ok()?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Splits a `ws://host:port/path` URL into `(authority, path)`. `wss://`
/// (TLS) is rejected - this crate carries no TLS transport. A missing
/// scheme treats the whole input as the authority with a `/` path.
pub fn parse_ws_url(url: &str) -> Result<(String, String), Error> {
    if let Some(_rest) = url.strip_prefix("wss://") {
        return Err(Error::Handshake(
            "wss:// (TLS) websocket client not supported".to_string(),
        ));
    }
    let rest = url.strip_prefix("ws://").unwrap_or(url);
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let path = if path.is_empty() { "/" } else { path };
    Ok((authority.to_string(), path.to_string()))
}

/// Server-side RFC 6455 opening handshake (§4.2). Reads the upgrade
/// request from `stream`, validates `Upgrade` / `Connection` /
/// `Sec-WebSocket-Version` / `Sec-WebSocket-Key`, and writes the `101
/// Switching Protocols` response with the negotiated accept token. On
/// success the stream is ready to wrap in [`WebSocket::server`].
pub fn server_accept<S: Read + Write>(stream: &mut S) -> Result<(), Error> {
    let head = read_handshake_head(stream)?;
    let upgrade = header_value(&head, b"upgrade")
        .ok_or_else(|| Error::Handshake("missing Upgrade header".into()))?;
    if !upgrade.eq_ignore_ascii_case(b"websocket") {
        return Err(Error::Handshake(format!(
            "bad Upgrade: {}",
            String::from_utf8_lossy(upgrade)
        )));
    }
    let connection = header_value(&head, b"connection")
        .ok_or_else(|| Error::Handshake("missing Connection header".into()))?;
    let has_upgrade_token = connection
        .split(|&b| b == b',')
        .any(|tok| tok.trim_ascii().eq_ignore_ascii_case(b"upgrade"));
    if !has_upgrade_token {
        return Err(Error::Handshake(format!(
            "bad Connection: {}",
            String::from_utf8_lossy(connection)
        )));
    }
    let version = header_value(&head, b"sec-websocket-version").unwrap_or(b"");
    if version.trim_ascii() != b"13" {
        return Err(Error::Handshake(format!(
            "bad version: {}",
            String::from_utf8_lossy(version)
        )));
    }
    let key = header_value(&head, b"sec-websocket-key")
        .ok_or_else(|| Error::Handshake("missing Sec-WebSocket-Key".into()))?;
    let token = compute_accept(&String::from_utf8_lossy(key));
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         upgrade: websocket\r\n\
         connection: Upgrade\r\n\
         sec-websocket-accept: {token}\r\n\r\n"
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}

/// Client-side RFC 6455 opening handshake. Writes the upgrade request
/// for `host`/`path`, reads the response, and confirms `101 Switching
/// Protocols`. On success the stream is ready to wrap in
/// [`WebSocket::client`].
pub fn client_handshake<S: Read + Write>(
    stream: &mut S,
    host: &str,
    path: &str,
) -> Result<(), Error> {
    let key = client_nonce();
    let request = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n"
    );
    stream.write_all(request.as_bytes())?;
    stream.flush()?;
    let head = read_handshake_head(stream)?;
    match response_status(&head) {
        Some(101) => Ok(()),
        Some(code) => Err(Error::Handshake(format!(
            "server refused upgrade: status {code}"
        ))),
        None => Err(Error::Handshake("malformed upgrade response".into())),
    }
}

/// A 24-char base64 `Sec-WebSocket-Key`. The value need only be a fresh
/// nonce - the framing layer's security does not rest on it - so a
/// time-seeded SplitMix64 over 16 bytes is sufficient.
fn client_nonce() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0xC0FFEE, |d| d.as_nanos() as u64)
        .wrapping_add(COUNTER.fetch_add(1, Ordering::Relaxed));
    let mut state = seed;
    let mut bytes = [0u8; 16];
    for b in &mut bytes {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *b = (state >> 56) as u8;
    }
    base64_encode(&bytes)
}

/// Full-duplex WebSocket connection wrapping a Read+Write stream
/// (typically a [`std::net::TcpStream`]).
pub struct WebSocket<S: Read + Write> {
    stream: S,
    /// Whether outbound frames should be masked (clients MUST, servers
    /// MUST NOT). Default `false` (server mode).
    pub mask_outbound: bool,
    /// Maximum payload size accepted on inbound frames.
    pub max_payload: usize,
    /// Whether incoming frames must be masked (server side).
    pub require_inbound_mask: bool,
    /// In-progress fragment buffer + opcode.
    fragment_buf: Vec<u8>,
    fragment_opcode: u8,
}

impl<S: Read + Write> WebSocket<S> {
    /// Wraps `stream` in server mode (no outbound masking; inbound
    /// frames required to be masked per RFC 6455 §5.1).
    pub fn server(stream: S) -> Self {
        Self {
            stream,
            mask_outbound: false,
            max_payload: 16 * 1024 * 1024,
            require_inbound_mask: true,
            fragment_buf: Vec::new(),
            fragment_opcode: 0,
        }
    }

    /// Wraps `stream` in client mode (outbound masking on; inbound
    /// masking forbidden per RFC).
    pub fn client(stream: S) -> Self {
        Self {
            stream,
            mask_outbound: true,
            max_payload: 16 * 1024 * 1024,
            require_inbound_mask: false,
            fragment_buf: Vec::new(),
            fragment_opcode: 0,
        }
    }

    /// Sends a text message.
    pub fn send_text(&mut self, payload: &str) -> Result<(), Error> {
        self.send_frame(OP_TEXT, payload.as_bytes(), true)
    }

    /// Sends a binary message.
    pub fn send_binary(&mut self, payload: &[u8]) -> Result<(), Error> {
        self.send_frame(OP_BINARY, payload, true)
    }

    /// Sends a ping frame.
    pub fn send_ping(&mut self, payload: &[u8]) -> Result<(), Error> {
        self.send_frame(OP_PING, payload, true)
    }

    /// Sends a pong frame (response to ping).
    pub fn send_pong(&mut self, payload: &[u8]) -> Result<(), Error> {
        self.send_frame(OP_PONG, payload, true)
    }

    /// Sends a close frame.
    pub fn send_close(&mut self, code: u16, reason: &str) -> Result<(), Error> {
        let mut payload = Vec::with_capacity(2 + reason.len());
        payload.extend_from_slice(&code.to_be_bytes());
        payload.extend_from_slice(reason.as_bytes());
        self.send_frame(OP_CLOSE, &payload, true)
    }

    /// Receives the next complete message, transparently reassembling
    /// fragments and auto-replying to pings.
    pub fn receive(&mut self) -> Result<Message, Error> {
        loop {
            let frame = self.read_frame()?;
            match frame.opcode {
                OP_CONTINUATION => {
                    self.fragment_buf.extend_from_slice(&frame.payload);
                    if frame.fin {
                        return self.assemble_fragments();
                    }
                }
                OP_TEXT | OP_BINARY => {
                    if frame.fin {
                        return Ok(if frame.opcode == OP_TEXT {
                            let s =
                                String::from_utf8(frame.payload).map_err(|_| Error::InvalidUtf8)?;
                            Message::Text(s)
                        } else {
                            Message::Binary(frame.payload)
                        });
                    }
                    self.fragment_buf = frame.payload;
                    self.fragment_opcode = frame.opcode;
                }
                OP_PING => {
                    // Auto-reply with pong; surface the ping to the caller.
                    let _ = self.send_pong(&frame.payload);
                    return Ok(Message::Ping(frame.payload));
                }
                OP_PONG => return Ok(Message::Pong(frame.payload)),
                OP_CLOSE => {
                    let (code, reason) = if frame.payload.len() >= 2 {
                        let code = u16::from_be_bytes([frame.payload[0], frame.payload[1]]);
                        let reason = String::from_utf8_lossy(&frame.payload[2..]).into_owned();
                        (code, reason)
                    } else {
                        (1005, String::new())
                    };
                    // Echo close per RFC 6455 §5.5.1 (best effort; if write
                    // fails the peer already closed).
                    let _ = self.send_close(code, &reason);
                    return Ok(Message::Close { code, reason });
                }
                op => return Err(Error::BadOpcode(op)),
            }
        }
    }

    fn assemble_fragments(&mut self) -> Result<Message, Error> {
        let bytes = std::mem::take(&mut self.fragment_buf);
        let op = std::mem::take(&mut self.fragment_opcode);
        match op {
            OP_TEXT => {
                let s = String::from_utf8(bytes).map_err(|_| Error::InvalidUtf8)?;
                Ok(Message::Text(s))
            }
            OP_BINARY => Ok(Message::Binary(bytes)),
            other => Err(Error::BadOpcode(other)),
        }
    }

    fn send_frame(&mut self, opcode: u8, payload: &[u8], fin: bool) -> Result<(), Error> {
        let mut hdr: Vec<u8> = Vec::with_capacity(16);
        let fin_byte = if fin { 0x80 } else { 0x00 };
        hdr.push(fin_byte | (opcode & 0x0F));
        let mask_byte = if self.mask_outbound { 0x80u8 } else { 0x00 };
        let len = payload.len();
        if len < LEN_MARKER_16 as usize {
            hdr.push(mask_byte | (len as u8));
        } else if u16::try_from(len).is_ok() {
            hdr.push(mask_byte | LEN_MARKER_16);
            hdr.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            hdr.push(mask_byte | LEN_MARKER_64);
            hdr.extend_from_slice(&(len as u64).to_be_bytes());
        }
        if self.mask_outbound {
            let mask = generate_mask();
            hdr.extend_from_slice(&mask);
            self.stream.write_all(&hdr)?;
            // Mask the payload in 4 KiB chunks to bound the intermediate
            // allocation. Apply mask in place into a scratch buffer.
            let mut scratch = [0u8; 4096];
            let mut i = 0usize;
            while i < payload.len() {
                let n = std::cmp::min(scratch.len(), payload.len() - i);
                for j in 0..n {
                    scratch[j] = payload[i + j] ^ mask[(i + j) % 4];
                }
                self.stream.write_all(&scratch[..n])?;
                i += n;
            }
        } else {
            self.stream.write_all(&hdr)?;
            self.stream.write_all(payload)?;
        }
        self.stream.flush()?;
        Ok(())
    }

    fn read_frame(&mut self) -> Result<RawFrame, Error> {
        let mut hdr = [0u8; 2];
        self.stream.read_exact(&mut hdr)?;
        let fin = (hdr[0] & 0x80) != 0;
        let opcode = hdr[0] & 0x0F;
        let masked = (hdr[1] & 0x80) != 0;
        let len_field = hdr[1] & 0x7F;
        let len: usize = match len_field {
            126 => {
                let mut ext = [0u8; 2];
                self.stream.read_exact(&mut ext)?;
                u16::from_be_bytes(ext) as usize
            }
            127 => {
                let mut ext = [0u8; 8];
                self.stream.read_exact(&mut ext)?;
                u64::from_be_bytes(ext) as usize
            }
            n => n as usize,
        };
        if len > self.max_payload {
            return Err(Error::PayloadTooLarge {
                limit: self.max_payload,
            });
        }
        if self.require_inbound_mask && !masked {
            return Err(Error::UnmaskedClientFrame);
        }
        let mut mask = [0u8; 4];
        if masked {
            self.stream.read_exact(&mut mask)?;
        }
        let mut payload = vec![0u8; len];
        if len > 0 {
            self.stream.read_exact(&mut payload)?;
        }
        if masked {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
        }
        Ok(RawFrame {
            fin,
            opcode,
            payload,
        })
    }

    /// Returns a mutable reference to the underlying stream.
    pub fn get_mut(&mut self) -> &mut S {
        &mut self.stream
    }

    /// Consumes the WebSocket and returns the underlying stream.
    pub fn into_inner(self) -> S {
        self.stream
    }
}

struct RawFrame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

fn generate_mask() -> [u8; 4] {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0xC0FFEE, |d| d.as_nanos() as u64);
    let mut state = nanos.wrapping_mul(0x9E3779B97F4A7C15);
    let mut out = [0u8; 4];
    for byte in &mut out {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *byte = (state >> 32) as u8;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn rfc_6455_handshake_example_round_trips() {
        // The example from RFC 6455 §1.3:
        // key = "dGhlIHNhbXBsZSBub25jZQ=="
        // accept = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        let token = compute_accept("dGhlIHNhbXBsZSBub25jZQ==");
        assert_eq!(token, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    struct PipeEnd {
        inbound: std::sync::Arc<parking_lot::Mutex<Vec<u8>>>,
        outbound: std::sync::Arc<parking_lot::Mutex<Vec<u8>>>,
    }

    impl Read for PipeEnd {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            loop {
                let mut g = self.inbound.lock();
                if g.is_empty() {
                    drop(g);
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    continue;
                }
                let n = std::cmp::min(g.len(), buf.len());
                buf[..n].copy_from_slice(&g[..n]);
                g.drain(..n);
                return Ok(n);
            }
        }
    }

    impl Write for PipeEnd {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.outbound.lock().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn pair() -> (PipeEnd, PipeEnd) {
        let c2s = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let s2c = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let client = PipeEnd {
            inbound: std::sync::Arc::clone(&s2c),
            outbound: std::sync::Arc::clone(&c2s),
        };
        let server = PipeEnd {
            inbound: std::sync::Arc::clone(&c2s),
            outbound: std::sync::Arc::clone(&s2c),
        };
        (client, server)
    }

    #[test]
    fn text_message_round_trip_client_to_server() {
        let (client_end, server_end) = pair();
        let mut server = WebSocket::server(server_end);
        let mut client = WebSocket::client(client_end);

        let handle = std::thread::spawn(move || server.receive());
        client.send_text("hello").unwrap();
        let msg = handle.join().unwrap().unwrap();
        assert_eq!(msg, Message::Text("hello".into()));
    }

    #[test]
    fn binary_message_round_trip() {
        let (client_end, server_end) = pair();
        let mut server = WebSocket::server(server_end);
        let mut client = WebSocket::client(client_end);

        let handle = std::thread::spawn(move || server.receive());
        client.send_binary(b"\x01\x02\x03\xff").unwrap();
        let msg = handle.join().unwrap().unwrap();
        assert_eq!(msg, Message::Binary(vec![1, 2, 3, 255]));
    }

    #[test]
    fn ping_triggers_pong_auto_reply() {
        let (client_end, server_end) = pair();
        let mut server = WebSocket::server(server_end);
        let mut client = WebSocket::client(client_end);

        let handle = std::thread::spawn(move || {
            // Server receives ping (auto-replies with pong) and returns
            // the ping Message.
            let msg = server.receive().unwrap();
            // Sleep briefly to let the pong land on the wire.
            std::thread::sleep(std::time::Duration::from_millis(20));
            msg
        });
        client.send_ping(b"PING").unwrap();
        // Client reads server's auto-pong.
        let pong = client.receive().unwrap();
        let handle_msg = handle.join().unwrap();
        assert_eq!(handle_msg, Message::Ping(b"PING".to_vec()));
        assert_eq!(pong, Message::Pong(b"PING".to_vec()));
    }

    #[test]
    fn close_frame_round_trip() {
        let (client_end, server_end) = pair();
        let mut server = WebSocket::server(server_end);
        let mut client = WebSocket::client(client_end);

        let handle = std::thread::spawn(move || server.receive());
        client.send_close(1000, "bye").unwrap();
        let msg = handle.join().unwrap().unwrap();
        match msg {
            Message::Close { code, reason } => {
                assert_eq!(code, 1000);
                assert_eq!(reason, "bye");
            }
            other => panic!("expected close, got {other:?}"),
        }
    }

    #[test]
    fn large_payload_uses_64bit_length_field() {
        // 70_000 bytes triggers the 64-bit length encoding.
        let payload = vec![0xAB; 70_000];
        let (client_end, server_end) = pair();
        let mut server = WebSocket::server(server_end);
        let mut client = WebSocket::client(client_end);

        let handle = std::thread::spawn(move || server.receive());
        client.send_binary(&payload).unwrap();
        let msg = handle.join().unwrap().unwrap();
        assert_eq!(msg, Message::Binary(payload));
    }

    #[test]
    fn server_rejects_unmasked_client_frame() {
        // Pretend client (unmasked) writing to server. Build a raw text
        // frame without masking.
        let frame = {
            let mut out = Vec::new();
            out.push(0x80 | OP_TEXT);
            out.push(5);
            out.extend_from_slice(b"hello");
            out
        };
        let mut ws = WebSocket::server(Cursor::new(frame));
        let err = ws.receive().unwrap_err();
        assert!(matches!(err, Error::UnmaskedClientFrame));
    }

    #[test]
    fn payload_above_limit_returns_error() {
        // Build a frame claiming 70_000 bytes, well above our test limit.
        let mut wire = Vec::new();
        wire.push(0x80 | OP_BINARY);
        wire.push(0x80 | 127); // masked + 8-byte length
        wire.extend_from_slice(&(70_000u64).to_be_bytes());
        wire.extend_from_slice(&[0u8; 4]); // mask
        // The size check fires before read_exact attempts the payload.
        let mut ws = WebSocket::server(Cursor::new(wire));
        ws.max_payload = 1024;
        let err = ws.receive().unwrap_err();
        assert!(matches!(err, Error::PayloadTooLarge { .. }));
    }

    #[test]
    fn sha1_known_vectors() {
        // FIPS 180-2 test vectors.
        assert_eq!(
            hex(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }

    fn hex(b: &[u8]) -> String {
        let mut s = String::with_capacity(b.len() * 2);
        for byte in b {
            s.push_str(&format!("{byte:02x}"));
        }
        s
    }
}
