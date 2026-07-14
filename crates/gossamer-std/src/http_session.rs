//! Runtime support for `std::http::session`.
//!
//! Pluggable HTTP session management with a signed-cookie backend
//! shipped by default. Sessions serialize as JSON, are framed as a
//! single cookie, and travel one of two wire shapes:
//!
//! - `SerializationMode::SignedOnly` - JSON payload + HMAC-SHA256
//!   tag. The payload is visible to the client; the tag prevents
//!   tampering.
//! - `SerializationMode::Encrypted` - AES-256-GCM ciphertext with
//!   integrated auth tag. The payload is opaque to the client and
//!   tampering is rejected by the AEAD open.
//!
//! Both shapes support **key rotation**: configure one active key
//! (used for sign / encrypt) and any number of legacy keys (tried
//! in order on verify / decrypt). When a legacy key opens a
//! cookie, the next `SessionStore::save` re-signs with the
//! active key, so rotation is transparent to callers.
//!
//! Invalid cookies - bad format, bad signature, bad ciphertext,
//! bad JSON - degrade to an **empty session** rather than a
//! visible error, so a tampered or stale cookie cannot lock a
//! user out of the application. Operators that need stricter
//! handling layer their own verification on top.
//!
//! The store writes a cookie to the response only when the
//! session is dirty (mutated by `set` / `remove` / `clear`) or
//! destroyed; pure reads pass through without setting headers so
//! responses stay cache-friendly.

#![forbid(unsafe_code)]

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::crypto;
use crate::errors::Error;
use crate::http::{Request, Response};
use crate::http_cookie::{CookieBuilder, SameSite, parse_cookie_header};

/// Default cookie name when [`SessionConfig`] does not override it.
const DEFAULT_COOKIE_NAME: &str = "gos_session";

/// Default session lifetime: 7 days.
const DEFAULT_MAX_AGE_SECS: i64 = 86_400 * 7;

/// Minimum HMAC-SHA256 signing-key length. HMAC technically accepts
/// shorter keys, but accepting them makes brute-force resistance depend on a
/// configuration mistake rather than the primitive's strength.
const MIN_HMAC_KEY_LEN: usize = 32;

/// Maximum encoded session cookie value accepted or emitted. Cookies have a
/// practical per-cookie browser limit around 4 KiB, but this larger parser cap
/// gives applications room for base64/encryption overhead while preventing an
/// attacker-controlled Cookie header from driving unbounded decode/JSON work.
const MAX_SESSION_COOKIE_BYTES: usize = 16 * 1024;

/// Maximum plaintext JSON session payload. Kept below the wire cap so a saved
/// session stays within a bounded, deployable cookie size after encoding.
const MAX_SESSION_PAYLOAD_BYTES: usize = 8 * 1024;

/// AES-256-GCM key length in bytes - only key length the underlying
/// AEAD primitive accepts in this build.
const AES_GCM_KEY_LEN: usize = 32;

/// AES-GCM nonce length in bytes.
const AES_GCM_NONCE_LEN: usize = 12;

/// RFC 1123 epoch string used when destroying a cookie.
const EPOCH_RFC1123: &str = "Thu, 01 Jan 1970 00:00:00 GMT";

/// Wire-shape selector for the signed-cookie backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SerializationMode {
    /// HMAC-SHA256 over the JSON payload. The payload is readable on
    /// the wire; tampering is rejected by the signature check.
    SignedOnly,
    /// AES-256-GCM(key) over the JSON payload. The payload is opaque
    /// to the client and the AEAD tag rejects tampering.
    Encrypted,
}

/// Configuration for [`SignedCookieStore`].
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Cookie name used on read and write. Default `gos_session`.
    pub cookie_name: String,
    /// Key material. The first entry is the **active** key (used
    /// for sign / encrypt). Remaining entries are legacy keys
    /// (tried in order during verify / decrypt for rotation).
    pub keys: Vec<Vec<u8>>,
    /// Wire-shape selector. Default [`SerializationMode::SignedOnly`].
    pub mode: SerializationMode,
    /// Cookie `Max-Age` in seconds. `0` => session-only cookie (no
    /// `Max-Age` attribute emitted, browser drops on tab close).
    /// Default 7 days.
    pub max_age_secs: i64,
    /// `SameSite` attribute. Default [`SameSite::Lax`].
    pub same_site: SameSite,
    /// `Secure` flag. Default `true` - production HTTPS only.
    pub secure: bool,
    /// `HttpOnly` flag. Default `true` - block JS access.
    pub http_only: bool,
    /// `Path` attribute. Default `"/"`.
    pub path: String,
    /// Optional `Domain` attribute. Default `None`.
    pub domain: Option<String>,
}

impl SessionConfig {
    /// Builds a signed-only session config with the given key.
    ///
    /// The key is used for HMAC-SHA256 and must be at least 32 bytes
    /// when the store is constructed.
    #[must_use]
    pub fn cookie(key: Vec<u8>) -> Self {
        Self {
            cookie_name: DEFAULT_COOKIE_NAME.to_string(),
            keys: vec![key],
            mode: SerializationMode::SignedOnly,
            max_age_secs: DEFAULT_MAX_AGE_SECS,
            same_site: SameSite::Lax,
            secure: true,
            http_only: true,
            path: "/".to_string(),
            domain: None,
        }
    }

    /// Builds an encrypted-cookie session config with the given key.
    ///
    /// The key must be exactly 32 bytes - AES-256-GCM is the only
    /// AEAD primitive wired through this build. Shorter / longer
    /// keys are rejected when the store is constructed, not here.
    #[must_use]
    pub fn encrypted_cookie(key: Vec<u8>) -> Self {
        let mut cfg = Self::cookie(key);
        cfg.mode = SerializationMode::Encrypted;
        cfg
    }

    /// Appends a legacy key. Legacy keys are tried after the active
    /// key during verify / decrypt; never used to sign new cookies.
    #[must_use]
    pub fn add_legacy_key(mut self, key: Vec<u8>) -> Self {
        self.keys.push(key);
        self
    }

    /// Overrides the cookie name.
    #[must_use]
    pub fn cookie_name(mut self, n: impl Into<String>) -> Self {
        self.cookie_name = n.into();
        self
    }

    /// Overrides the `Max-Age` in seconds; `0` makes the cookie
    /// session-only (no `Max-Age` attribute).
    #[must_use]
    pub fn max_age_secs(mut self, secs: i64) -> Self {
        self.max_age_secs = secs;
        self
    }

    /// Overrides the `SameSite` attribute.
    #[must_use]
    pub fn same_site(mut self, ss: SameSite) -> Self {
        self.same_site = ss;
        self
    }

    /// Overrides the `Secure` flag.
    #[must_use]
    pub fn secure(mut self, on: bool) -> Self {
        self.secure = on;
        self
    }

    /// Overrides the `HttpOnly` flag.
    #[must_use]
    pub fn http_only(mut self, on: bool) -> Self {
        self.http_only = on;
        self
    }

    /// Overrides the `Path` attribute.
    #[must_use]
    pub fn path(mut self, p: impl Into<String>) -> Self {
        self.path = p.into();
        self
    }

    /// Sets the `Domain` attribute.
    #[must_use]
    pub fn domain(mut self, d: impl Into<String>) -> Self {
        self.domain = Some(d.into());
        self
    }

    fn active_key(&self) -> &[u8] {
        // SAFETY-style note: SignedCookieStore::new validates that
        // `keys` is non-empty, so this is the standard accessor
        // for downstream codepaths.
        &self.keys[0]
    }
}

/// In-memory session payload. Serializes to JSON for the wire.
#[derive(Debug, Default, Clone)]
pub struct Session {
    data: serde_json::Map<String, serde_json::Value>,
    dirty: bool,
    destroy: bool,
}

/// Authenticated on-wire session frame. Keeping expiry inside the signed or
/// encrypted payload prevents a copied cookie from being replayed after a
/// browser-only `Max-Age` would have elapsed.
#[derive(Serialize, Deserialize)]
struct SessionEnvelope {
    version: u8,
    issued_at: i64,
    expires_at: Option<i64>,
    data: serde_json::Map<String, serde_json::Value>,
}

impl Session {
    /// Constructs a fresh empty session.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the value at `key` decoded into `T`. `Ok(None)` when
    /// the key is absent; `Err` when the stored JSON cannot be
    /// decoded as `T`.
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, Error> {
        match self.data.get(key) {
            None => Ok(None),
            Some(v) => serde_json::from_value(v.clone())
                .map(Some)
                .map_err(|e| Error::new(format!("session: decode {key}: {e}"))),
        }
    }

    /// String-typed convenience accessor.
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.data.get(key).and_then(serde_json::Value::as_str)
    }

    /// Integer-typed convenience accessor.
    #[must_use]
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.data.get(key).and_then(serde_json::Value::as_i64)
    }

    /// Inserts or overwrites the value at `key`. Marks the session
    /// dirty so the next save writes the cookie.
    pub fn set<T: Serialize>(&mut self, key: impl Into<String>, value: T) -> Result<(), Error> {
        let v = serde_json::to_value(value)
            .map_err(|e| Error::new(format!("session: encode value: {e}")))?;
        self.data.insert(key.into(), v);
        self.dirty = true;
        Ok(())
    }

    /// Removes `key` if present. Marks the session dirty.
    pub fn remove(&mut self, key: &str) {
        if self.data.remove(key).is_some() {
            self.dirty = true;
        }
    }

    /// Empties the session payload. Marks dirty.
    pub fn clear(&mut self) {
        if !self.data.is_empty() {
            self.dirty = true;
        }
        self.data.clear();
    }

    /// Marks the session for destruction. On the next save the
    /// response emits a deletion cookie (empty value, `Max-Age=0`,
    /// expired `Expires`).
    pub fn destroy(&mut self) {
        self.destroy = true;
        self.dirty = true;
        self.data.clear();
    }

    /// `true` when no entries are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// `true` when the session has been mutated (the next save
    /// will write a cookie).
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// `true` when `destroy()` has been called.
    #[must_use]
    pub fn is_destroyed(&self) -> bool {
        self.destroy
    }
}

/// Pluggable backend for session load / save. The stock 0.8.0
/// implementation is [`SignedCookieStore`]; Redis / SQL / in-memory
/// backends are future work.
pub trait SessionStore: Send + Sync + 'static {
    /// Reads any session cookie off the request and returns the
    /// decoded payload, or a fresh empty session when the cookie is
    /// missing / invalid.
    fn load(&self, request: &Request) -> Session;

    /// Writes a `Set-Cookie` header to the response when the session
    /// is dirty or destroyed; no-op otherwise.
    fn save(&self, session: &Session, response: &mut Response);
}

/// Stateless signed-cookie session backend. The full session
/// payload travels on the cookie; the server stores only the keys.
pub struct SignedCookieStore {
    config: SessionConfig,
}

impl SignedCookieStore {
    /// Builds a store from the given configuration. Panics-free -
    /// configuration errors (no keys, wrong key length for the
    /// chosen mode) surface here so handler code can rely on the
    /// store always succeeding on `load` / `save`.
    ///
    /// Returns an error when:
    /// - `config.keys` is empty.
    /// - Any signed-only key is shorter than 32 bytes.
    /// - `mode = Encrypted` and any configured key is not exactly
    ///   32 bytes (AES-256-GCM requirement).
    /// - Cookie attributes violate the strict cookie contract.
    pub fn try_new(config: SessionConfig) -> Result<Self, Error> {
        if config.keys.is_empty() {
            return Err(Error::new("session: at least one key is required"));
        }
        if config.max_age_secs < 0 {
            return Err(Error::new("session: max_age_secs must not be negative"));
        }
        for (i, key) in config.keys.iter().enumerate() {
            match config.mode {
                SerializationMode::SignedOnly if key.len() < MIN_HMAC_KEY_LEN => {
                    return Err(Error::new(format!(
                        "session: signed mode requires keys of at least {MIN_HMAC_KEY_LEN} bytes; \
                         key #{i} is {} bytes",
                        key.len()
                    )));
                }
                SerializationMode::Encrypted if key.len() != AES_GCM_KEY_LEN => {
                    return Err(Error::new(format!(
                        "session: encrypted mode requires {AES_GCM_KEY_LEN}-byte keys; \
                         key #{i} is {} bytes",
                        key.len()
                    )));
                }
                _ => {}
            }
        }
        let mut cookie = crate::http_cookie::Cookie::builder(&config.cookie_name, "validation")
            .path(config.path.clone())
            .http_only(config.http_only)
            .secure(config.secure)
            .same_site(config.same_site);
        if let Some(domain) = &config.domain {
            cookie = cookie.domain(domain.clone());
        }
        cookie
            .try_build()
            .map_err(|e| Error::wrap(e, "session: invalid cookie configuration"))?;
        Ok(Self { config })
    }

    /// Same as [`Self::try_new`] but panics on configuration error.
    /// Convenience for top-of-`main` setup where a misconfigured
    /// session backend is unrecoverable anyway.
    ///
    /// Prefer [`Self::try_new`] in code that wants to surface the
    /// error.
    #[must_use]
    pub fn new(config: SessionConfig) -> Self {
        match Self::try_new(config) {
            Ok(s) => s,
            Err(e) => panic!("SignedCookieStore::new: {}", e.message()),
        }
    }

    /// Borrows the active config.
    #[must_use]
    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    fn encode(&self, payload: &[u8]) -> Result<String, Error> {
        match self.config.mode {
            SerializationMode::SignedOnly => {
                let b64_payload = b64url_encode(payload);
                let mac =
                    crypto::hmac::sha256_mac(self.config.active_key(), b64_payload.as_bytes());
                let b64_sig = b64url_encode(&mac);
                Ok(format!("{b64_payload}.{b64_sig}"))
            }
            SerializationMode::Encrypted => {
                let nonce = crypto::rand::bytes(AES_GCM_NONCE_LEN)
                    .map_err(|e| Error::new(format!("session: nonce: {}", e.message())))?;
                let ciphertext =
                    crypto::aead::aes_256_gcm_seal(self.config.active_key(), &nonce, payload, &[])
                        .map_err(|e| Error::new(format!("session: seal: {}", e.message())))?;
                let b64_nonce = b64url_encode(&nonce);
                let b64_ct = b64url_encode(&ciphertext);
                Ok(format!("{b64_nonce}.{b64_ct}"))
            }
        }
    }

    fn decode(&self, wire: &str) -> Option<Vec<u8>> {
        if wire.len() > MAX_SESSION_COOKIE_BYTES {
            return None;
        }
        let (left, right) = wire.split_once('.')?;
        let left_bytes = b64url_decode(left).ok()?;
        let right_bytes = b64url_decode(right).ok()?;
        match self.config.mode {
            SerializationMode::SignedOnly => {
                // Re-MAC the b64url payload string and constant-time-compare with
                // the supplied tag, trying active key first then legacy keys.
                let payload_b64_bytes = left.as_bytes();
                for key in &self.config.keys {
                    let mac = crypto::hmac::sha256_mac(key, payload_b64_bytes);
                    if crypto::subtle::constant_time_eq(&mac, &right_bytes) {
                        return (left_bytes.len() <= MAX_SESSION_PAYLOAD_BYTES)
                            .then_some(left_bytes);
                    }
                }
                None
            }
            SerializationMode::Encrypted => {
                if left_bytes.len() != AES_GCM_NONCE_LEN {
                    return None;
                }
                for key in &self.config.keys {
                    if key.len() != AES_GCM_KEY_LEN {
                        continue;
                    }
                    if let Ok(plain) =
                        crypto::aead::aes_256_gcm_open(key, &left_bytes, &right_bytes, &[])
                    {
                        return (plain.len() <= MAX_SESSION_PAYLOAD_BYTES).then_some(plain);
                    }
                }
                None
            }
        }
    }

    fn build_session_cookie(&self, value: String) -> String {
        let mut builder = CookieBuilder::from(crate::http_cookie::Cookie::new(
            &self.config.cookie_name,
            value,
        ))
        .path(self.config.path.clone())
        .http_only(self.config.http_only)
        .secure(self.config.secure)
        .same_site(self.config.same_site);
        if let Some(d) = &self.config.domain {
            builder = builder.domain(d.clone());
        }
        if self.config.max_age_secs > 0 {
            builder = builder.max_age(self.config.max_age_secs);
        }
        builder.build().to_header_value()
    }

    fn build_deletion_cookie(&self) -> String {
        let mut builder = CookieBuilder::from(crate::http_cookie::Cookie::new(
            &self.config.cookie_name,
            "",
        ))
        .path(self.config.path.clone())
        .http_only(self.config.http_only)
        .secure(self.config.secure)
        .same_site(self.config.same_site)
        .max_age(0)
        .expires(EPOCH_RFC1123);
        if let Some(d) = &self.config.domain {
            builder = builder.domain(d.clone());
        }
        builder.build().to_header_value()
    }

    fn encode_session(&self, data: serde_json::Map<String, serde_json::Value>) -> Option<String> {
        let issued_at = unix_time_secs()?;
        let expires_at = if self.config.max_age_secs == 0 {
            None
        } else {
            issued_at.checked_add(self.config.max_age_secs)
        };
        let payload = serde_json::to_vec(&SessionEnvelope {
            version: 1,
            issued_at,
            expires_at,
            data,
        })
        .ok()?;
        if payload.len() > MAX_SESSION_PAYLOAD_BYTES {
            return None;
        }
        self.encode(&payload).ok()
    }

    fn decode_session(&self, wire: &str) -> Option<Session> {
        let plain = self.decode(wire)?;
        let envelope = serde_json::from_slice::<SessionEnvelope>(&plain).ok()?;
        if envelope.version != 1 || envelope.issued_at < 0 {
            return None;
        }
        if let Some(expires_at) = envelope.expires_at
            && unix_time_secs()? >= expires_at
        {
            return None;
        }
        Some(Session {
            data: envelope.data,
            dirty: false,
            destroy: false,
        })
    }
}

impl SessionStore for SignedCookieStore {
    fn load(&self, request: &Request) -> Session {
        let Some(header) = request.headers.get("cookie") else {
            return Session::new();
        };
        for (name, value) in parse_cookie_header(header) {
            if name == self.config.cookie_name {
                if let Some(session) = self.decode_session(&value) {
                    return session;
                }
                // Right name, bad cookie: degrade to empty.
                return Session::new();
            }
        }
        Session::new()
    }

    fn save(&self, session: &Session, response: &mut Response) {
        if !session.is_dirty() && !session.is_destroyed() {
            return;
        }
        let header_value = if session.is_destroyed() {
            self.build_deletion_cookie()
        } else {
            let Some(value) = self.encode_session(session.data.clone()) else {
                return;
            };
            self.build_session_cookie(value)
        };
        // Multiple Set-Cookie headers can stack on a single response;
        // the BTreeMap-backed Headers map keeps the last value when
        // a duplicate key arrives, which is the right shape here
        // (saving twice overwrites the first emission).
        response.headers.insert("set-cookie", &header_value);
    }
}

fn unix_time_secs() -> Option<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
}

/// Convenience adapter: `load`, run the handler with a mutable
/// session, `save`. The session lifecycle is fully contained in
/// the closure scope; no thread-locals or hidden state.
pub fn with_session<F, R>(
    store: &dyn SessionStore,
    request: &Request,
    response: &mut Response,
    f: F,
) -> R
where
    F: FnOnce(&mut Session) -> R,
{
    let mut session = store.load(request);
    let out = f(&mut session);
    store.save(&session, response);
    out
}

// -------- URL-safe base64 (RFC 4648 §5, no padding) --------

const URL_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64url_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut chunks = input.chunks_exact(3);
    for chunk in chunks.by_ref() {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        out.push(URL_ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(URL_ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(URL_ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        out.push(URL_ALPHABET[(n & 0x3f) as usize] as char);
    }
    let rem = chunks.remainder();
    match rem.len() {
        1 => {
            let n = u32::from(rem[0]) << 16;
            out.push(URL_ALPHABET[((n >> 18) & 0x3f) as usize] as char);
            out.push(URL_ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        }
        2 => {
            let n = (u32::from(rem[0]) << 16) | (u32::from(rem[1]) << 8);
            out.push(URL_ALPHABET[((n >> 18) & 0x3f) as usize] as char);
            out.push(URL_ALPHABET[((n >> 12) & 0x3f) as usize] as char);
            out.push(URL_ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        }
        _ => {}
    }
    out
}

fn b64url_decode(input: &str) -> Result<Vec<u8>, Error> {
    let bytes = input.as_bytes();
    let remainder = bytes.len() % 4;
    let pad = match remainder {
        0 => 0,
        2 => 2,
        3 => 1,
        _ => return Err(Error::new("b64url: invalid length")),
    };
    // RFC 4648 requires unused bits in an unpadded final quantum to be zero.
    // Reject alternate spellings that otherwise decode to the same bytes.
    let unused_bits = match remainder {
        2 => 0x0f,
        3 => 0x03,
        _ => 0,
    };
    if unused_bits != 0
        && let Some(last) = bytes.last().and_then(|byte| url_index(*byte))
        && last & unused_bits != 0
    {
        return Err(Error::new("b64url: non-canonical trailing bits"));
    }
    let total = bytes.len() + pad;
    let mut padded = Vec::with_capacity(total);
    padded.extend_from_slice(bytes);
    padded.extend(std::iter::repeat_n(b'=', pad));
    let mut out = Vec::with_capacity(total / 4 * 3);
    for chunk in padded.chunks(4) {
        let mut values = [0u32; 4];
        let mut padding_count = 0;
        for (i, byte) in chunk.iter().enumerate() {
            if *byte == b'=' {
                padding_count += 1;
                values[i] = 0;
            } else {
                values[i] = url_index(*byte).ok_or_else(|| Error::new("b64url: bad character"))?;
            }
        }
        let n = (values[0] << 18) | (values[1] << 12) | (values[2] << 6) | values[3];
        out.push((n >> 16) as u8);
        if padding_count < 2 {
            out.push((n >> 8) as u8);
        }
        if padding_count < 1 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

fn url_index(byte: u8) -> Option<u32> {
    Some(u32::from(match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'-' => 62,
        b'_' => 63,
        _ => return None,
    }))
}

// Re-export so the from-conversion below stays local. The `Cookie`
// type defines `builder()` only with both name and value, but we
// need to take ownership of a fresh `Cookie` and wrap it; this
// `From` impl is the bridge.
impl From<crate::http_cookie::Cookie> for CookieBuilder {
    fn from(inner: crate::http_cookie::Cookie) -> Self {
        let mut b = crate::http_cookie::Cookie::builder(inner.name.clone(), inner.value.clone());
        if let Some(d) = inner.domain {
            b = b.domain(d);
        }
        if let Some(p) = inner.path {
            b = b.path(p);
        }
        if let Some(m) = inner.max_age {
            b = b.max_age(m);
        }
        if let Some(e) = inner.expires {
            b = b.expires(e);
        }
        if inner.http_only {
            b = b.http_only(true);
        }
        if inner.secure {
            b = b.secure(true);
        }
        if let Some(s) = inner.same_site {
            b = b.same_site(s);
        }
        b
    }
}

// Touch the silence-the-unused-warning constant so any future
// reorganization that drops it surfaces the dead-code lint.
const _: usize = MIN_HMAC_KEY_LEN;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use crate::http::{Headers, Method, Request, Response, StatusCode};

    fn signing_key() -> Vec<u8> {
        // 32-byte deterministic test key.
        (0u8..32).collect()
    }

    fn aes_key(seed: u8) -> Vec<u8> {
        vec![seed; 32]
    }

    fn fresh_request_with_cookie(cookie_header: Option<&str>) -> Request {
        let mut headers = Headers::new();
        if let Some(h) = cookie_header {
            headers.insert("cookie", h);
        }
        Request {
            method: Method::Get,
            path: "/".to_string(),
            query: String::new(),
            headers,
            body: Vec::new(),
            context: Context::background(),
            trailers: None,
        }
    }

    fn fresh_response() -> Response {
        Response::text(StatusCode::OK, "")
    }

    fn extract_set_cookie(response: &Response) -> Option<String> {
        response.headers.get("set-cookie").map(str::to_string)
    }

    fn cookie_value_for(name: &str, header_value: &str) -> Option<String> {
        for (n, v) in parse_cookie_header(header_value) {
            if n == name {
                return Some(v);
            }
        }
        None
    }

    fn make_signed_store() -> SignedCookieStore {
        SignedCookieStore::new(SessionConfig::cookie(signing_key()))
    }

    fn make_encrypted_store() -> SignedCookieStore {
        SignedCookieStore::new(SessionConfig::encrypted_cookie(aes_key(0xA5)))
    }

    fn roundtrip_through(store: &SignedCookieStore, mutate: impl FnOnce(&mut Session)) -> Session {
        let req = fresh_request_with_cookie(None);
        let mut resp = fresh_response();
        let mut session = store.load(&req);
        mutate(&mut session);
        store.save(&session, &mut resp);
        let set_cookie = extract_set_cookie(&resp).expect("save must emit cookie when dirty");
        let cookie_value = cookie_value_for(&store.config.cookie_name, &set_cookie)
            .expect("cookie name must match");
        let cookie_header = format!("{}={}", store.config.cookie_name, cookie_value);
        let req2 = fresh_request_with_cookie(Some(&cookie_header));
        store.load(&req2)
    }

    #[test]
    fn signed_roundtrip_preserves_set_value() {
        let store = make_signed_store();
        let loaded = roundtrip_through(&store, |s| {
            s.set("user_id", 42_i64).unwrap();
            s.set("name", "ada").unwrap();
        });
        assert_eq!(loaded.get_i64("user_id"), Some(42));
        assert_eq!(loaded.get_str("name"), Some("ada"));
    }

    #[test]
    fn encrypted_roundtrip_preserves_set_value() {
        let store = make_encrypted_store();
        let loaded = roundtrip_through(&store, |s| {
            s.set("secret", "shhh").unwrap();
        });
        assert_eq!(loaded.get_str("secret"), Some("shhh"));
    }

    #[test]
    fn tampered_signature_yields_empty_session() {
        let store = make_signed_store();
        let req = fresh_request_with_cookie(None);
        let mut resp = fresh_response();
        let mut s = store.load(&req);
        s.set("k", "v").unwrap();
        store.save(&s, &mut resp);
        let set_cookie = extract_set_cookie(&resp).unwrap();
        let value = cookie_value_for(&store.config.cookie_name, &set_cookie).unwrap();
        let (payload, sig) = value.split_once('.').unwrap();
        // Flip a bit in the last sig character - pick a different
        // letter from the URL-safe alphabet.
        let mut tampered_sig: String = sig.to_string();
        let last = tampered_sig.pop().unwrap();
        let replacement = if last == 'A' { 'B' } else { 'A' };
        tampered_sig.push(replacement);
        let bad = format!("{payload}.{tampered_sig}");
        let cookie_name = &store.config.cookie_name;
        let req2 = fresh_request_with_cookie(Some(&format!("{cookie_name}={bad}")));
        let loaded = store.load(&req2);
        assert!(loaded.is_empty());
    }

    #[test]
    fn tampered_ciphertext_yields_empty_session() {
        let store = make_encrypted_store();
        let req = fresh_request_with_cookie(None);
        let mut resp = fresh_response();
        let mut s = store.load(&req);
        s.set("token", "abcdef").unwrap();
        store.save(&s, &mut resp);
        let set_cookie = extract_set_cookie(&resp).unwrap();
        let value = cookie_value_for(&store.config.cookie_name, &set_cookie).unwrap();
        let (nonce_b64, ct_b64) = value.split_once('.').unwrap();
        // Tamper a byte in the MIDDLE of the ciphertext, not the
        // last base64 char. With no-padding base64url, low bits of
        // the trailing char can land in padding bits that the
        // decoder discards, leaving the decoded ciphertext (and
        // hence the AEAD verification) unchanged. A middle char
        // always alters at least one decoded byte.
        let mut tampered_ct: Vec<char> = ct_b64.chars().collect();
        assert!(tampered_ct.len() >= 4, "ciphertext too short");
        let mid = tampered_ct.len() / 2;
        let original = tampered_ct[mid];
        tampered_ct[mid] = if original == 'A' { 'B' } else { 'A' };
        let tampered_ct: String = tampered_ct.into_iter().collect();
        let bad = format!("{nonce_b64}.{tampered_ct}");
        let cookie_name = &store.config.cookie_name;
        let req2 = fresh_request_with_cookie(Some(&format!("{cookie_name}={bad}")));
        let loaded = store.load(&req2);
        assert!(loaded.is_empty());
    }

    #[test]
    fn key_rotation_loads_with_legacy_then_resigns() {
        // Sign with old key.
        let old_key = vec![0x11u8; 32];
        let store_old = SignedCookieStore::new(SessionConfig::cookie(old_key.clone()));
        let req = fresh_request_with_cookie(None);
        let mut resp = fresh_response();
        let mut s = store_old.load(&req);
        s.set("uid", 7_i64).unwrap();
        store_old.save(&s, &mut resp);
        let cookie_value = cookie_value_for(
            &store_old.config.cookie_name,
            &extract_set_cookie(&resp).unwrap(),
        )
        .unwrap();

        // Rotate: new active key, old key demoted to legacy.
        let new_key = vec![0x22u8; 32];
        let store_new =
            SignedCookieStore::new(SessionConfig::cookie(new_key.clone()).add_legacy_key(old_key));
        let req2 = fresh_request_with_cookie(Some(&format!(
            "{}={}",
            store_new.config.cookie_name, cookie_value
        )));
        let loaded = store_new.load(&req2);
        assert_eq!(loaded.get_i64("uid"), Some(7));

        // Resave: must re-sign with new active key. Verify by
        // confirming a store with only the new key can read it.
        let mut loaded_dirty = loaded;
        loaded_dirty.set("uid", 7_i64).unwrap();
        let mut resp2 = fresh_response();
        store_new.save(&loaded_dirty, &mut resp2);
        let resigned = cookie_value_for(
            &store_new.config.cookie_name,
            &extract_set_cookie(&resp2).unwrap(),
        )
        .unwrap();

        let store_new_only = SignedCookieStore::new(SessionConfig::cookie(new_key));
        let req3 = fresh_request_with_cookie(Some(&format!(
            "{}={}",
            store_new_only.config.cookie_name, resigned
        )));
        assert_eq!(store_new_only.load(&req3).get_i64("uid"), Some(7));
    }

    #[test]
    fn save_is_noop_when_not_dirty() {
        let store = make_signed_store();
        let req = fresh_request_with_cookie(None);
        let mut resp = fresh_response();
        let session = store.load(&req);
        store.save(&session, &mut resp);
        assert!(extract_set_cookie(&resp).is_none());
    }

    #[test]
    fn destroy_emits_deletion_cookie() {
        let store = make_signed_store();
        let req = fresh_request_with_cookie(None);
        let mut resp = fresh_response();
        let mut session = store.load(&req);
        session.destroy();
        store.save(&session, &mut resp);
        let set_cookie = extract_set_cookie(&resp).expect("destroy must emit cookie");
        assert!(set_cookie.contains("Max-Age=0"));
        assert!(set_cookie.contains("Expires=Thu, 01 Jan 1970 00:00:00 GMT"));
        let value = cookie_value_for(&store.config.cookie_name, &set_cookie).unwrap();
        assert_eq!(value, "");
    }

    #[test]
    fn cookie_name_override_round_trips_under_new_name() {
        let store =
            SignedCookieStore::new(SessionConfig::cookie(signing_key()).cookie_name("my_app_sess"));
        let loaded = roundtrip_through(&store, |s| {
            s.set("ok", true).unwrap();
        });
        assert_eq!(loaded.get::<bool>("ok").unwrap(), Some(true));
    }

    #[test]
    fn multiple_cookies_disambiguated_by_name() {
        let store = make_signed_store();
        // First produce a real signed cookie value.
        let req = fresh_request_with_cookie(None);
        let mut resp = fresh_response();
        let mut s = store.load(&req);
        s.set("k", 99_i64).unwrap();
        store.save(&s, &mut resp);
        let real = cookie_value_for(
            &store.config.cookie_name,
            &extract_set_cookie(&resp).unwrap(),
        )
        .unwrap();

        // Build a request header with several cookies of different
        // names - only the one matching the configured name should
        // be consulted.
        let cookie_header = format!(
            "other=xyz; {}={}; another=hello",
            store.config.cookie_name, real
        );
        let req2 = fresh_request_with_cookie(Some(&cookie_header));
        let loaded = store.load(&req2);
        assert_eq!(loaded.get_i64("k"), Some(99));
    }

    #[test]
    fn max_age_zero_omits_max_age_attribute() {
        let store = SignedCookieStore::new(SessionConfig::cookie(signing_key()).max_age_secs(0));
        let req = fresh_request_with_cookie(None);
        let mut resp = fresh_response();
        let mut s = store.load(&req);
        s.set("x", 1_i64).unwrap();
        store.save(&s, &mut resp);
        let set_cookie = extract_set_cookie(&resp).unwrap();
        assert!(
            !set_cookie.contains("Max-Age="),
            "session-only cookie must omit Max-Age, got: {set_cookie}"
        );
    }

    #[test]
    fn custom_struct_serializes_and_decodes() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Prefs {
            theme: String,
            count: i64,
            on: bool,
        }
        let store = make_signed_store();
        let original = Prefs {
            theme: "dark".to_string(),
            count: 12,
            on: true,
        };
        let loaded = roundtrip_through(&store, |s| {
            s.set("prefs", &original).unwrap();
        });
        let decoded: Prefs = loaded.get("prefs").unwrap().unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn long_payload_round_trips_intact() {
        let store = make_signed_store();
        // ~1 KB JSON payload across many keys.
        let loaded = roundtrip_through(&store, |s| {
            for i in 0..50 {
                s.set(format!("key_{i}"), format!("value-with-padding-{i:08}"))
                    .unwrap();
            }
        });
        for i in 0..50 {
            let key = format!("key_{i}");
            assert_eq!(
                loaded.get_str(&key),
                Some(format!("value-with-padding-{i:08}")).as_deref()
            );
        }
    }

    #[test]
    fn encrypted_rotation_legacy_key_decrypts() {
        let old = aes_key(0x11);
        let store_old = SignedCookieStore::new(SessionConfig::encrypted_cookie(old.clone()));
        let req = fresh_request_with_cookie(None);
        let mut resp = fresh_response();
        let mut s = store_old.load(&req);
        s.set("uid", 7_i64).unwrap();
        store_old.save(&s, &mut resp);
        let cookie_value = cookie_value_for(
            &store_old.config.cookie_name,
            &extract_set_cookie(&resp).unwrap(),
        )
        .unwrap();

        let new = aes_key(0x22);
        let store_new =
            SignedCookieStore::new(SessionConfig::encrypted_cookie(new).add_legacy_key(old));
        let req2 = fresh_request_with_cookie(Some(&format!(
            "{}={}",
            store_new.config.cookie_name, cookie_value
        )));
        let loaded = store_new.load(&req2);
        assert_eq!(loaded.get_i64("uid"), Some(7));
    }

    #[test]
    fn try_new_rejects_empty_keys() {
        let mut cfg = SessionConfig::cookie(signing_key());
        cfg.keys.clear();
        assert!(SignedCookieStore::try_new(cfg).is_err());
    }

    #[test]
    fn try_new_rejects_short_signed_and_legacy_keys() {
        assert!(SignedCookieStore::try_new(SessionConfig::cookie(vec![7; 31])).is_err());
        let cfg = SessionConfig::cookie(signing_key()).add_legacy_key(vec![9; 31]);
        assert!(SignedCookieStore::try_new(cfg).is_err());
    }

    #[test]
    fn oversized_session_cookie_is_rejected_without_json_decode() {
        let store = make_signed_store();
        let huge = "a".repeat(MAX_SESSION_COOKIE_BYTES + 1);
        let req = fresh_request_with_cookie(Some(&format!("{}={huge}", store.config.cookie_name)));
        assert!(store.load(&req).is_empty());
    }

    #[test]
    fn expired_or_legacy_signed_payload_is_rejected() {
        let store = make_signed_store();
        let expired = SessionEnvelope {
            version: 1,
            issued_at: 1,
            expires_at: Some(2),
            data: serde_json::Map::new(),
        };
        let expired_wire = store
            .encode(&serde_json::to_vec(&expired).unwrap())
            .expect("encode expired cookie");
        let expired_req = fresh_request_with_cookie(Some(&format!(
            "{}={expired_wire}",
            store.config.cookie_name
        )));
        assert!(store.load(&expired_req).is_empty());

        let legacy_wire = store.encode(br#"{"uid":7}"#).expect("encode legacy cookie");
        let legacy_req =
            fresh_request_with_cookie(Some(&format!("{}={legacy_wire}", store.config.cookie_name)));
        assert!(store.load(&legacy_req).is_empty());
    }

    #[test]
    fn try_new_rejects_wrong_encrypted_key_length() {
        let cfg = SessionConfig::encrypted_cookie(vec![0u8; 16]);
        assert!(SignedCookieStore::try_new(cfg).is_err());
    }

    #[test]
    fn with_session_threads_load_save() {
        let store = make_signed_store();
        let req = fresh_request_with_cookie(None);
        let mut resp = fresh_response();
        let out = with_session(&store, &req, &mut resp, |session| {
            session.set("k", "v").unwrap();
            session.get_str("k").unwrap().to_string()
        });
        assert_eq!(out, "v");
        assert!(extract_set_cookie(&resp).is_some());
    }

    #[test]
    fn cookie_attributes_match_config() {
        let store = SignedCookieStore::new(
            SessionConfig::cookie(signing_key())
                .same_site(SameSite::Strict)
                .secure(true)
                .http_only(true)
                .path("/app")
                .domain("example.com"),
        );
        let req = fresh_request_with_cookie(None);
        let mut resp = fresh_response();
        let mut s = store.load(&req);
        s.set("k", 1_i64).unwrap();
        store.save(&s, &mut resp);
        let h = extract_set_cookie(&resp).unwrap();
        assert!(h.contains("Path=/app"), "{h}");
        assert!(h.contains("Domain=example.com"), "{h}");
        assert!(h.contains("HttpOnly"), "{h}");
        assert!(h.contains("Secure"), "{h}");
        assert!(h.contains("SameSite=Strict"), "{h}");
    }

    #[test]
    fn b64url_round_trips_random_lengths() {
        for n in 0..50 {
            let input: Vec<u8> = (0..n).map(|i| (i * 7) as u8).collect();
            let s = b64url_encode(&input);
            // URL-safe alphabet only.
            assert!(
                s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            );
            let back = b64url_decode(&s).unwrap();
            assert_eq!(back, input);
        }
    }

    #[test]
    fn b64url_rejects_noncanonical_trailing_bits() {
        // `AB` and `AAB` decode to the same bytes as `AA` and `AAA` when
        // their ignored base64 padding bits are not validated.
        for encoded in ["AB", "AAB"] {
            assert!(b64url_decode(encoded).is_err(), "{encoded}");
        }
    }

    #[test]
    fn malformed_cookie_value_yields_empty_session() {
        let store = make_signed_store();
        let req = fresh_request_with_cookie(Some(&format!(
            "{}={}",
            store.config.cookie_name, "not-a-real-cookie"
        )));
        let loaded = store.load(&req);
        assert!(loaded.is_empty());
    }
}
