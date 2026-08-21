//! An SMTP client, enough to send the mail an application owes its users:
//! a password reset, an address verification, a magic link, a security
//! notification.
//!
//! Deliberately one transaction per call. A pool, a queue, retries, and
//! bounce handling are an application's policy, not a client's, and belong
//! in a package built on this.
//!
//! Transport follows what the server offers: port 465 is implicit TLS,
//! anything else starts plaintext and upgrades through `STARTTLS` when the
//! server advertises it. Credentials are only ever sent over TLS - a
//! server that offers no encryption is refused rather than handed a
//! password, because the alternative is leaking it to anyone on the path.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

/// How long each read and write may take. A mail server that stops
/// answering must not hold the goroutine that asked it forever.
const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// One addressed message.
pub struct Message<'a> {
    /// Envelope sender, also the `From` header.
    pub from: &'a str,
    /// Envelope recipient, also the `To` header. Comma-separated for
    /// several, each of which gets its own `RCPT TO`.
    pub to: &'a str,
    /// `Subject` header.
    pub subject: &'a str,
    /// Plain-text body.
    pub body: &'a str,
}

/// Credentials for `AUTH`, or `None` to send unauthenticated.
pub struct Credentials<'a> {
    /// The account name.
    pub username: &'a str,
    /// The account secret.
    pub password: &'a str,
}

/// Either half of the connection, before and after the TLS upgrade.
enum Transport {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Transport {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(buf),
            Self::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Transport {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(buf),
            Self::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls(s) => s.flush(),
        }
    }
}

impl Transport {
    fn is_tls(&self) -> bool {
        matches!(self, Self::Tls(_))
    }
}

/// One SMTP conversation.
struct Session {
    io: BufReader<Transport>,
}

impl Session {
    /// Reads one complete reply, which may span several `250-` lines, and
    /// answers `(code, text)`.
    fn read_reply(&mut self) -> Result<(u16, String), String> {
        let mut text = String::new();
        loop {
            let mut line = String::new();
            let read = self
                .io
                .read_line(&mut line)
                .map_err(|e| format!("smtp: read: {e}"))?;
            if read == 0 {
                return Err("smtp: server closed the connection".to_string());
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.len() < 3 {
                return Err(format!("smtp: malformed reply {trimmed:?}"));
            }
            let code: u16 = trimmed[..3]
                .parse()
                .map_err(|_| format!("smtp: malformed reply code in {trimmed:?}"))?;
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(trimmed[3..].trim_start_matches(['-', ' ']));
            // A hyphen after the code means another line follows.
            if trimmed.as_bytes().get(3) != Some(&b'-') {
                return Ok((code, text));
            }
        }
    }

    fn write_line(&mut self, line: &str) -> Result<(), String> {
        let io = self.io.get_mut();
        io.write_all(line.as_bytes())
            .and_then(|()| io.write_all(b"\r\n"))
            .and_then(|()| io.flush())
            .map_err(|e| format!("smtp: write: {e}"))
    }

    /// Sends `line` and requires a reply in the 2xx range.
    fn command(&mut self, line: &str, what: &str) -> Result<String, String> {
        self.write_line(line)?;
        let (code, text) = self.read_reply()?;
        if (200..300).contains(&code) {
            Ok(text)
        } else {
            Err(format!("smtp: {what} refused with {code}: {text}"))
        }
    }
}

/// Base64, for `AUTH PLAIN` / `AUTH LOGIN`.
fn b64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(triple >> 18) as usize & 0x3f] as char);
        out.push(TABLE[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

/// The host part of `host:port`, for the TLS server name and `EHLO`.
fn host_of(addr: &str) -> &str {
    addr.rsplit_once(':').map_or(addr, |(host, _)| host)
}

/// The port part of `host:port`, or 25.
fn port_of(addr: &str) -> u16 {
    addr.rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
        .unwrap_or(25)
}

/// Wraps `stream` in TLS for `host`.
fn upgrade(
    stream: TcpStream,
    host: &str,
    config: &Arc<rustls::ClientConfig>,
) -> Result<Transport, String> {
    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| format!("smtp: {host:?} is not a valid TLS server name"))?;
    let connection = rustls::ClientConnection::new(Arc::clone(config), server_name)
        .map_err(|e| format!("smtp: tls handshake: {e}"))?;
    Ok(Transport::Tls(Box::new(rustls::StreamOwned::new(
        connection, stream,
    ))))
}

/// A header value with any CR or LF removed.
///
/// A newline in a caller-supplied subject or address would end the header
/// and let the rest be read as more headers, or as the body - the header
/// injection that turns a "send a reset link" endpoint into an open relay.
fn header_safe(value: &str) -> String {
    value.replace(['\r', '\n'], " ")
}

/// The message as it goes on the wire, dot-stuffed so a line that is just
/// `.` cannot end the DATA block early.
fn render(message: &Message<'_>, host: &str) -> String {
    let mut out = String::with_capacity(message.body.len() + 256);
    out.push_str(&format!("From: {}\r\n", header_safe(message.from)));
    out.push_str(&format!("To: {}\r\n", header_safe(message.to)));
    out.push_str(&format!("Subject: {}\r\n", header_safe(message.subject)));
    out.push_str(&format!(
        "Message-ID: <{}.{}@{}>\r\n",
        crate::clock::wall_ms(),
        std::process::id(),
        header_safe(host)
    ));
    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    out.push_str("\r\n");
    for line in message.body.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.starts_with('.') {
            out.push('.');
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out
}

/// Sends one message through `addr`, authenticating when credentials are
/// supplied.
pub fn send(
    addr: &str,
    message: &Message<'_>,
    credentials: Option<&Credentials<'_>>,
) -> Result<(), String> {
    let host = host_of(addr).to_string();
    let stream = TcpStream::connect(addr).map_err(|e| format!("smtp: connect {addr}: {e}"))?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)))
        .map_err(|e| format!("smtp: set timeouts: {e}"))?;

    let config = tls_config();
    // Port 465 speaks TLS from the first byte; every other port starts in
    // the clear and upgrades if the server offers it.
    let transport = if port_of(addr) == 465 {
        upgrade(stream, &host, &config)?
    } else {
        Transport::Plain(stream)
    };
    let mut session = Session {
        io: BufReader::new(transport),
    };

    let (code, text) = session.read_reply()?;
    if code != 220 {
        return Err(format!("smtp: greeting was {code}: {text}"));
    }
    let mut capabilities = session.command(&format!("EHLO {host}"), "EHLO")?;

    if !session.io.get_ref().is_tls() && capabilities.to_ascii_uppercase().contains("STARTTLS") {
        session.command("STARTTLS", "STARTTLS")?;
        let Transport::Plain(stream) = session.io.into_inner() else {
            return Err("smtp: STARTTLS on an already-encrypted connection".to_string());
        };
        session = Session {
            io: BufReader::new(upgrade(stream, &host, &config)?),
        };
        // The capability list before the upgrade is not authenticated, so
        // it is asked for again over TLS - which is where AUTH is read from.
        capabilities = session.command(&format!("EHLO {host}"), "EHLO after STARTTLS")?;
    }

    if let Some(credentials) = credentials {
        if !session.io.get_ref().is_tls() {
            return Err(format!(
                "smtp: {addr} offers no encryption; refusing to send credentials in the clear"
            ));
        }
        authenticate(&mut session, credentials, &capabilities)?;
    }

    session.command(
        &format!("MAIL FROM:<{}>", header_safe(message.from)),
        "MAIL FROM",
    )?;
    for recipient in message.to.split(',') {
        let recipient = recipient.trim();
        if recipient.is_empty() {
            continue;
        }
        session.command(&format!("RCPT TO:<{}>", header_safe(recipient)), "RCPT TO")?;
    }
    session.write_line("DATA")?;
    let (code, text) = session.read_reply()?;
    if code != 354 {
        return Err(format!("smtp: DATA refused with {code}: {text}"));
    }
    {
        let io = session.io.get_mut();
        io.write_all(render(message, &host).as_bytes())
            .and_then(|()| io.write_all(b".\r\n"))
            .and_then(|()| io.flush())
            .map_err(|e| format!("smtp: write body: {e}"))?;
    }
    let (code, text) = session.read_reply()?;
    if !(200..300).contains(&code) {
        return Err(format!("smtp: message refused with {code}: {text}"));
    }
    // A server that will not say goodbye has still accepted the message.
    let _ = session.command("QUIT", "QUIT");
    Ok(())
}

/// Authenticates with whichever mechanism the server advertised.
fn authenticate(
    session: &mut Session,
    credentials: &Credentials<'_>,
    capabilities: &str,
) -> Result<(), String> {
    let advertised = capabilities.to_ascii_uppercase();
    if advertised.contains("PLAIN") {
        let mut secret = Vec::new();
        secret.push(0);
        secret.extend_from_slice(credentials.username.as_bytes());
        secret.push(0);
        secret.extend_from_slice(credentials.password.as_bytes());
        session.command(&format!("AUTH PLAIN {}", b64(&secret)), "AUTH PLAIN")?;
        return Ok(());
    }
    if advertised.contains("LOGIN") {
        session.write_line("AUTH LOGIN")?;
        let (code, text) = session.read_reply()?;
        if code != 334 {
            return Err(format!("smtp: AUTH LOGIN refused with {code}: {text}"));
        }
        session.write_line(&b64(credentials.username.as_bytes()))?;
        let (code, text) = session.read_reply()?;
        if code != 334 {
            return Err(format!(
                "smtp: AUTH LOGIN username refused with {code}: {text}"
            ));
        }
        session.command(&b64(credentials.password.as_bytes()), "AUTH LOGIN password")?;
        return Ok(());
    }
    Err("smtp: server advertises no AUTH mechanism this client speaks (PLAIN, LOGIN)".to_string())
}

/// The trust anchors an SMTP connection verifies against - the same ones
/// the HTTP client and `net::TcpStream::start_tls` use.
fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: std::sync::OnceLock<Arc<rustls::ClientConfig>> = std::sync::OnceLock::new();
    Arc::clone(CONFIG.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        // ring always supports the default protocol versions; the only
        // error path is a provider missing them, which cannot happen here.
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring provider supports default protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        Arc::new(config)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_known_vectors() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foo"), "Zm9v");
        assert_eq!(b64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn a_newline_cannot_be_smuggled_into_a_header() {
        // A subject carrying CRLF would otherwise end the header and let
        // the rest read as more headers or as the body.
        let message = Message {
            from: "a@example.com",
            to: "b@example.com",
            subject: "hi\r\nBcc: victim@example.com",
            body: "text",
        };
        let wire = render(&message, "example.com");
        assert!(wire.contains("Subject: hi  Bcc: victim@example.com\r\n"));
        assert_eq!(wire.matches("Bcc:").count(), 1);
        assert!(!wire.contains("\r\nBcc:"));
    }

    #[test]
    fn a_lone_dot_line_is_stuffed_so_it_cannot_end_the_body() {
        let message = Message {
            from: "a@example.com",
            to: "b@example.com",
            subject: "s",
            body: "one\n.\ntwo",
        };
        let wire = render(&message, "example.com");
        assert!(wire.contains("\r\n..\r\n"), "got {wire}");
    }

    #[test]
    fn host_and_port_split_at_the_last_colon() {
        assert_eq!(host_of("smtp.example.com:587"), "smtp.example.com");
        assert_eq!(port_of("smtp.example.com:587"), 587);
        assert_eq!(port_of("smtp.example.com"), 25);
        assert_eq!(port_of("smtp.example.com:465"), 465);
    }
}
