// Runtime support for `std::net::ip` — IP address types and utilities.
//
// Wraps Rust's `std::net::{IpAddr, Ipv4Addr, Ipv6Addr}`. All parsing
// and comparison goes through Rust's battle-tested implementation.

#![forbid(unsafe_code)]

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Parsed IP address — either v4 or v6.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ip {
    /// An IPv4 address.
    V4(Ipv4Addr),
    /// An IPv6 address.
    V6(Ipv6Addr),
}

impl fmt::Display for Ip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V4(a) => a.fmt(f),
            Self::V6(a) => a.fmt(f),
        }
    }
}

impl Ip {
    /// Parses a dotted-decimal IPv4 or colon-hex IPv6 string.
    pub fn parse(s: &str) -> Result<Self, String> {
        s.parse::<IpAddr>()
            .map(|a| match a {
                IpAddr::V4(v4) => Self::V4(v4),
                IpAddr::V6(v6) => Self::V6(v6),
            })
            .map_err(|e| format!("net::ip: {e}"))
    }

    /// `true` if this is an IPv4 address.
    #[must_use]
    pub fn is_v4(&self) -> bool {
        matches!(self, Self::V4(_))
    }

    /// `true` if this is an IPv6 address.
    #[must_use]
    pub fn is_v6(&self) -> bool {
        matches!(self, Self::V6(_))
    }

    /// `true` if this is a loopback address (127.0.0.1 / `::1`).
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        match self {
            Self::V4(a) => a.is_loopback(),
            Self::V6(a) => a.is_loopback(),
        }
    }

    /// `true` if this is a private / RFC-1918 address.
    #[must_use]
    pub fn is_private(&self) -> bool {
        match self {
            Self::V4(a) => a.is_private(),
            Self::V6(a) => a.is_loopback() || a.is_unique_local(),
        }
    }

    /// `true` if this is an unspecified address (0.0.0.0 / ::).
    #[must_use]
    pub fn is_unspecified(&self) -> bool {
        match self {
            Self::V4(a) => a.is_unspecified(),
            Self::V6(a) => a.is_unspecified(),
        }
    }

    /// `true` if this is a multicast address.
    #[must_use]
    pub fn is_multicast(&self) -> bool {
        match self {
            Self::V4(a) => a.is_multicast(),
            Self::V6(a) => a.is_multicast(),
        }
    }

    /// Returns the raw octets as a `Vec<u8>`.
    #[must_use]
    pub fn octets(&self) -> Vec<u8> {
        match self {
            Self::V4(a) => a.octets().to_vec(),
            Self::V6(a) => a.octets().to_vec(),
        }
    }
}

/// Parses an IPv4/IPv6 string into an `Ip`.
pub fn parse(s: &str) -> Result<Ip, String> {
    Ip::parse(s)
}

/// `true` if `s` is a syntactically valid IPv4 or IPv6 address.
#[must_use]
pub fn is_valid(s: &str) -> bool {
    s.parse::<IpAddr>().is_ok()
}

/// `true` if `s` is a syntactically valid IPv4 address.
#[must_use]
pub fn is_v4(s: &str) -> bool {
    s.parse::<Ipv4Addr>().is_ok()
}

/// `true` if `s` is a syntactically valid IPv6 address.
#[must_use]
pub fn is_v6(s: &str) -> bool {
    s.parse::<Ipv6Addr>().is_ok()
}

/// Converts an `Ip` to its canonical string representation.
#[must_use]
pub fn to_string(ip: &Ip) -> String {
    ip.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_v4() {
        let ip = parse("192.168.1.1").unwrap();
        assert!(ip.is_v4());
        assert!(!ip.is_v6());
        assert_eq!(ip.to_string(), "192.168.1.1");
    }

    #[test]
    fn parse_v6() {
        let ip = parse("::1").unwrap();
        assert!(ip.is_v6());
        assert!(ip.is_loopback());
    }

    #[test]
    fn loopback() {
        assert!(parse("127.0.0.1").unwrap().is_loopback());
        assert!(parse("::1").unwrap().is_loopback());
    }

    #[test]
    fn private() {
        assert!(parse("10.0.0.1").unwrap().is_private());
        assert!(parse("192.168.0.1").unwrap().is_private());
        assert!(!parse("8.8.8.8").unwrap().is_private());
    }

    #[test]
    fn multicast() {
        assert!(parse("224.0.0.1").unwrap().is_multicast());
    }

    #[test]
    fn octets_v4() {
        let ip = parse("1.2.3.4").unwrap();
        assert_eq!(ip.octets(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn invalid_returns_err() {
        assert!(parse("not-an-ip").is_err());
    }

    #[test]
    fn is_valid_checks() {
        assert!(is_valid("127.0.0.1"));
        assert!(is_valid("::1"));
        assert!(!is_valid("999.0.0.1"));
    }
}
