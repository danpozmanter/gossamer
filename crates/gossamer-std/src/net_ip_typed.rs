// `std::net::netip` - typed IP address operations.
//
// Backed by Rust's `std::net::IpAddr`. Returns "" / -1 sentinels on
// parse failure to keep the runtime ABI simple (the value lives
// fully in i64 / String pairs that the c_abi shims can hand back).
//
// For each function the compiled tier wires through
// `gossamer_runtime::c_abi::gos_rt_netip_*`.

#![forbid(unsafe_code)]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// `true` iff `s` parses as a v4 or v6 IP address.
#[must_use]
pub fn is_valid(s: &str) -> bool {
    s.parse::<IpAddr>().is_ok()
}

/// `true` iff `s` parses as a v4 address.
#[must_use]
pub fn is_v4(s: &str) -> bool {
    s.parse::<Ipv4Addr>().is_ok()
}

/// `true` iff `s` parses as a v6 address.
#[must_use]
pub fn is_v6(s: &str) -> bool {
    s.parse::<Ipv6Addr>().is_ok()
}

/// `true` iff `s` parses as a loopback address (`127.0.0.1` / `::1`).
#[must_use]
pub fn is_loopback(s: &str) -> bool {
    s.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// `true` iff `s` parses as the unspecified address (0.0.0.0 / ::).
#[must_use]
pub fn is_unspecified(s: &str) -> bool {
    s.parse::<IpAddr>().is_ok_and(|ip| ip.is_unspecified())
}

/// `true` iff `s` parses as a multicast address.
#[must_use]
pub fn is_multicast(s: &str) -> bool {
    s.parse::<IpAddr>().is_ok_and(|ip| ip.is_multicast())
}

/// `true` iff `s` parses as an RFC 1918 / ULA private address.
#[must_use]
pub fn is_private(s: &str) -> bool {
    match s.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.is_private(),
        Ok(IpAddr::V6(v6)) => v6.segments()[0] & 0xfe00 == 0xfc00,
        Err(_) => false,
    }
}

/// Canonical, lowercase form of `s`, or empty string on parse failure.
#[must_use]
pub fn normalize(s: &str) -> String {
    match s.parse::<IpAddr>() {
        Ok(ip) => ip.to_string(),
        Err(_) => String::new(),
    }
}

/// Host portion of `addr:port`, or empty on parse failure.
#[must_use]
pub fn host_of(s: &str) -> String {
    match s.parse::<SocketAddr>() {
        Ok(a) => a.ip().to_string(),
        Err(_) => String::new(),
    }
}

/// Port portion of `addr:port`, or -1 on parse failure.
#[must_use]
pub fn port_of(s: &str) -> i64 {
    match s.parse::<SocketAddr>() {
        Ok(a) => i64::from(a.port()),
        Err(_) => -1,
    }
}

/// Compose an `addr:port` string from components. Returns "" on
/// failure (invalid host or port out of range).
#[must_use]
pub fn join_addr_port(host: &str, port: i64) -> String {
    let Ok(ip) = host.parse::<IpAddr>() else {
        return String::new();
    };
    let Ok(p) = u16::try_from(port) else {
        return String::new();
    };
    SocketAddr::new(ip, p).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_valid_accepts_v4_and_v6() {
        assert!(is_valid("127.0.0.1"));
        assert!(is_valid("::1"));
    }

    #[test]
    fn is_valid_rejects_garbage() {
        assert!(!is_valid("not-an-ip"));
        assert!(!is_valid(""));
    }

    #[test]
    fn is_v4_distinguishes() {
        assert!(is_v4("10.0.0.1"));
        assert!(!is_v4("::1"));
        assert!(is_v6("::1"));
        assert!(!is_v6("10.0.0.1"));
    }

    #[test]
    fn is_loopback_detects() {
        assert!(is_loopback("127.0.0.1"));
        assert!(is_loopback("::1"));
        assert!(!is_loopback("10.0.0.1"));
    }

    #[test]
    fn is_private_rfc1918() {
        assert!(is_private("10.0.0.1"));
        assert!(is_private("192.168.1.1"));
        assert!(is_private("172.16.0.1"));
        assert!(!is_private("8.8.8.8"));
    }

    #[test]
    fn normalize_v6() {
        // Canonical lowercase + shortened form.
        let n = normalize("2001:0db8:0000:0000:0000:0000:0000:0001");
        assert_eq!(n, "2001:db8::1");
    }

    #[test]
    fn parse_addr_port_v4() {
        assert_eq!(host_of("127.0.0.1:8080"), "127.0.0.1");
        assert_eq!(port_of("127.0.0.1:8080"), 8080);
    }

    #[test]
    fn join_addr_port_v6_brackets() {
        // SocketAddr emits the bracketed v6 form.
        let s = join_addr_port("::1", 8080);
        assert_eq!(s, "[::1]:8080");
    }
}
