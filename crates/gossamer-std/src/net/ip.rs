// Runtime support for `std::net::ip` - IP address types and utilities.
//
// Wraps Rust's `std::net::{IpAddr, Ipv4Addr, Ipv6Addr}`. All parsing
// and comparison goes through Rust's battle-tested implementation.

#![forbid(unsafe_code)]

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Parsed IP address - either v4 or v6.
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

// --- CIDR / IpNet ---------------------------------------------------

/// A CIDR-style IP network: address + prefix length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpNet {
    /// IPv4 network.
    V4 {
        /// Network address (host bits zeroed).
        base: Ipv4Addr,
        /// Prefix length in bits (0..=32).
        prefix: u8,
    },
    /// IPv6 network.
    V6 {
        /// Network address (host bits zeroed).
        base: Ipv6Addr,
        /// Prefix length in bits (0..=128).
        prefix: u8,
    },
}

impl IpNet {
    /// Parses `addr/prefix` (e.g. `"10.0.0.0/8"`,
    /// `"2001:db8::/32"`).
    pub fn parse(s: &str) -> Result<Self, String> {
        let (addr_part, prefix_part) = s
            .split_once('/')
            .ok_or_else(|| format!("CIDR missing slash: {s}"))?;
        let prefix: u8 = prefix_part
            .parse()
            .map_err(|_| format!("CIDR bad prefix: {prefix_part}"))?;
        if let Ok(v4) = addr_part.parse::<Ipv4Addr>() {
            if prefix > 32 {
                return Err(format!("IPv4 prefix > 32: {prefix}"));
            }
            let mask = if prefix == 0 {
                0u32
            } else {
                u32::MAX.checked_shl(u32::from(32 - prefix)).unwrap_or(0)
            };
            let bits = u32::from(v4) & mask;
            return Ok(Self::V4 {
                base: Ipv4Addr::from(bits),
                prefix,
            });
        }
        if let Ok(v6) = addr_part.parse::<Ipv6Addr>() {
            if prefix > 128 {
                return Err(format!("IPv6 prefix > 128: {prefix}"));
            }
            let bits = u128::from(v6);
            let mask = if prefix == 0 {
                0u128
            } else {
                u128::MAX.checked_shl(u32::from(128 - prefix)).unwrap_or(0)
            };
            let masked = bits & mask;
            return Ok(Self::V6 {
                base: Ipv6Addr::from(masked),
                prefix,
            });
        }
        Err(format!("CIDR bad address: {addr_part}"))
    }

    /// Returns `true` if `addr` falls inside this network.
    #[must_use]
    pub fn contains(&self, addr: &Ip) -> bool {
        match (self, addr) {
            (Self::V4 { base, prefix }, Ip::V4(a)) => {
                let mask = if *prefix == 0 {
                    0u32
                } else {
                    u32::MAX.checked_shl(u32::from(32 - *prefix)).unwrap_or(0)
                };
                let addr_u32 = u32::from(*a);
                let base_u32 = u32::from(*base);
                (addr_u32 & mask) == (base_u32 & mask)
            }
            (Self::V6 { base, prefix }, Ip::V6(a)) => {
                let mask = if *prefix == 0 {
                    0u128
                } else {
                    u128::MAX.checked_shl(u32::from(128 - *prefix)).unwrap_or(0)
                };
                let bits = u128::from(*a);
                let base_u128 = u128::from(*base);
                (bits & mask) == (base_u128 & mask)
            }
            _ => false,
        }
    }

    /// Returns the prefix length.
    #[must_use]
    pub fn prefix_len(&self) -> u8 {
        match self {
            Self::V4 { prefix, .. } | Self::V6 { prefix, .. } => *prefix,
        }
    }

    /// Renders as `addr/prefix`.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::V4 { base, prefix } => format!("{base}/{prefix}"),
            Self::V6 { base, prefix } => format!("{base}/{prefix}"),
        }
    }
}

impl From<Ipv4Addr> for Ip {
    fn from(v: Ipv4Addr) -> Self {
        Self::V4(v)
    }
}

impl From<Ipv6Addr> for Ip {
    fn from(v: Ipv6Addr) -> Self {
        Self::V6(v)
    }
}

#[cfg(test)]
mod cidr_tests {
    use super::*;

    #[test]
    fn parses_ipv4_cidr() {
        let n = IpNet::parse("10.1.2.3/8").unwrap();
        assert_eq!(n.render(), "10.0.0.0/8");
    }

    #[test]
    fn parses_ipv6_cidr() {
        let n = IpNet::parse("2001:db8::1/32").unwrap();
        assert_eq!(n.prefix_len(), 32);
    }

    #[test]
    fn ipv4_contains_in_range() {
        let n = IpNet::parse("10.0.0.0/8").unwrap();
        assert!(n.contains(&Ip::V4("10.1.2.3".parse().unwrap())));
        assert!(n.contains(&Ip::V4("10.255.255.255".parse().unwrap())));
        assert!(!n.contains(&Ip::V4("11.0.0.0".parse().unwrap())));
    }

    #[test]
    fn ipv4_contains_uses_full_prefix() {
        let n = IpNet::parse("192.168.1.0/24").unwrap();
        assert!(n.contains(&Ip::V4("192.168.1.42".parse().unwrap())));
        assert!(!n.contains(&Ip::V4("192.168.2.0".parse().unwrap())));
    }

    #[test]
    fn ipv6_contains_in_range() {
        let n = IpNet::parse("2001:db8::/32").unwrap();
        assert!(n.contains(&Ip::V6("2001:db8::1".parse().unwrap())));
        assert!(n.contains(&Ip::V6("2001:db8:ffff::".parse().unwrap())));
        assert!(!n.contains(&Ip::V6("2001:db9::".parse().unwrap())));
    }

    #[test]
    fn prefix_zero_matches_everything() {
        let n4 = IpNet::parse("0.0.0.0/0").unwrap();
        let n6 = IpNet::parse("::/0").unwrap();
        assert!(n4.contains(&Ip::V4("127.0.0.1".parse().unwrap())));
        assert!(n6.contains(&Ip::V6("::1".parse().unwrap())));
    }

    #[test]
    fn cross_family_does_not_contain() {
        let n = IpNet::parse("10.0.0.0/8").unwrap();
        assert!(!n.contains(&Ip::V6("::ffff:10.0.0.1".parse().unwrap())));
    }

    #[test]
    fn invalid_prefix_is_rejected() {
        assert!(IpNet::parse("10.0.0.0/33").is_err());
        assert!(IpNet::parse("::/129").is_err());
    }

    #[test]
    fn missing_slash_is_rejected() {
        assert!(IpNet::parse("10.0.0.0").is_err());
    }
}
