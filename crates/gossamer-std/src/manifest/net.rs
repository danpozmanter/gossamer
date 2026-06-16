#![allow(
    unused_imports,
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::items_after_statements,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::option_if_let_else,
    clippy::match_same_arms,
    clippy::if_not_else,
    clippy::single_match_else,
    clippy::needless_pass_by_value,
    clippy::manual_let_else,
    clippy::redundant_else,
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::map_unwrap_or,
    clippy::struct_excessive_bools,
    clippy::module_name_repetitions,
    clippy::unnecessary_wraps,
    clippy::large_enum_variant,
    clippy::if_same_then_else,
    clippy::single_match,
    clippy::useless_conversion,
    clippy::needless_borrows_for_generic_args,
    clippy::let_and_return,
    clippy::needless_collect,
    clippy::elidable_lifetime_names,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::missing_const_for_fn,
    clippy::needless_range_loop,
    clippy::ptr_arg,
    clippy::ptr_as_ptr,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::single_call_fn,
    clippy::unused_self,
    clippy::range_plus_one,
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::cast_ptr_alignment,
    clippy::manual_assert,
    clippy::manual_string_new,
    clippy::match_bool,
    clippy::nonminimal_bool,
    clippy::redundant_pattern_matching,
    clippy::useless_let_if_seq
)]
//! Static manifest of every registered stdlib module.
//! Each stdlib milestone extends this table with
//! the modules it adds. Entries are listed in phase-introduction order
//! so a `gos doc` walk renders modules in the same sequence as the
//! implementation plan.

#![forbid(unsafe_code)]
use crate::registry::{StdItem, StdItemKind, StdModule};

use super::*;

pub const NET_URL: StdModule = StdModule {
    path: "std::net::url",
    summary: "URL parsing, rendering, and query escaping.",
    items: &[
        StdItem {
            name: "Url",
            kind: StdItemKind::Type,
            doc: "Parsed URL.",
        },
        StdItem {
            name: "query_escape",
            kind: StdItemKind::Function,
            doc: "Percent-encodes a query parameter.",
        },
        StdItem {
            name: "query_unescape",
            kind: StdItemKind::Function,
            doc: "Inverse of `query_escape`.",
        },
        StdItem {
            name: "path_escape",
            kind: StdItemKind::Function,
            doc: "Percent-encodes a URL path segment.",
        },
        StdItem {
            name: "path_unescape",
            kind: StdItemKind::Function,
            doc: "Inverse of `path_escape`.",
        },
    ],
};

pub const NET: StdModule = StdModule {
    path: "std::net",
    summary: "TCP/UDP networking primitives.",
    items: &[
        StdItem {
            name: "TcpListener",
            kind: StdItemKind::Type,
            doc: "Accepts incoming TCP connections.",
        },
        StdItem {
            name: "TcpStream",
            kind: StdItemKind::Type,
            doc: "Bidirectional TCP byte stream.",
        },
        StdItem {
            name: "UdpSocket",
            kind: StdItemKind::Type,
            doc: "Bound UDP socket for datagram I/O.",
        },
        StdItem {
            name: "resolve",
            kind: StdItemKind::Function,
            doc: "Resolves a hostname to a list of IP addresses.",
        },
        StdItem {
            name: "lookup",
            kind: StdItemKind::Function,
            doc: "Resolves a hostname to its IP addresses (alias of resolve).",
        },
    ],
};

pub const NET_IP: StdModule = StdModule {
    path: "std::net::ip",
    summary: "String-level IPv4 / IPv6 parsing and classification helpers.",
    items: &[
        StdItem {
            name: "parse",
            kind: StdItemKind::Function,
            doc: "Parses an IP string, returning its canonical form or None.",
        },
        StdItem {
            name: "is_valid",
            kind: StdItemKind::Function,
            doc: "Reports whether the string is a valid v4 or v6 IP.",
        },
        StdItem {
            name: "is_v4",
            kind: StdItemKind::Function,
            doc: "Reports whether the string is a valid v4 IP.",
        },
        StdItem {
            name: "is_v6",
            kind: StdItemKind::Function,
            doc: "Reports whether the string is a valid v6 IP.",
        },
        StdItem {
            name: "to_string",
            kind: StdItemKind::Function,
            doc: "Canonical lowercase string form of the IP.",
        },
        StdItem {
            name: "is_loopback",
            kind: StdItemKind::Function,
            doc: "Reports whether the IP is a loopback address.",
        },
        StdItem {
            name: "is_private",
            kind: StdItemKind::Function,
            doc: "Reports whether the IP is in a private range.",
        },
        StdItem {
            name: "is_multicast",
            kind: StdItemKind::Function,
            doc: "Reports whether the IP is a multicast address.",
        },
        StdItem {
            name: "is_unspecified",
            kind: StdItemKind::Function,
            doc: "Reports whether the IP is the unspecified address.",
        },
        StdItem {
            name: "octets",
            kind: StdItemKind::Function,
            doc: "Byte octets of the IP as a Vec.",
        },
    ],
};

pub const NETIP: StdModule = StdModule {
    path: "std::net::netip",
    summary: "Typed IP-address parsing, classification, and addr:port helpers (Go's net/netip shape).",
    items: &[
        StdItem {
            name: "is_valid",
            kind: StdItemKind::Function,
            doc: "Return true iff the string parses as a v4 or v6 IP.",
        },
        StdItem {
            name: "is_v4",
            kind: StdItemKind::Function,
            doc: "Return true iff the string parses as a v4 IP.",
        },
        StdItem {
            name: "is_v6",
            kind: StdItemKind::Function,
            doc: "Return true iff the string parses as a v6 IP.",
        },
        StdItem {
            name: "is_loopback",
            kind: StdItemKind::Function,
            doc: "Return true iff the string parses as a loopback IP (127.0.0.1 / ::1).",
        },
        StdItem {
            name: "is_unspecified",
            kind: StdItemKind::Function,
            doc: "Return true iff the string parses as the unspecified IP (0.0.0.0 / ::).",
        },
        StdItem {
            name: "is_multicast",
            kind: StdItemKind::Function,
            doc: "Return true iff the string parses as a multicast IP.",
        },
        StdItem {
            name: "is_private",
            kind: StdItemKind::Function,
            doc: "Return true iff the IP is RFC1918 (v4) or ULA fc00::/7 (v6).",
        },
        StdItem {
            name: "normalize",
            kind: StdItemKind::Function,
            doc: "Canonical lowercase form of the IP, or empty string on parse failure.",
        },
        StdItem {
            name: "host_of",
            kind: StdItemKind::Function,
            doc: "Host portion of an addr:port string, or empty on parse failure.",
        },
        StdItem {
            name: "port_of",
            kind: StdItemKind::Function,
            doc: "Port portion of an addr:port string, or -1 on parse failure.",
        },
        StdItem {
            name: "join_addr_port",
            kind: StdItemKind::Function,
            doc: "Compose an addr:port string from host and port, or empty on failure.",
        },
    ],
};

pub const MIME: StdModule = StdModule {
    path: "std::mime",
    summary: "RFC 2045 media type parsing, parameter extraction, and extension lookup.",
    items: &[
        StdItem {
            name: "parse",
            kind: StdItemKind::Function,
            doc: "Canonical `type/subtype` form of the input, or empty on parse failure.",
        },
        StdItem {
            name: "top",
            kind: StdItemKind::Function,
            doc: "Top-level type (e.g. `text`) of a media type, or empty.",
        },
        StdItem {
            name: "sub",
            kind: StdItemKind::Function,
            doc: "Subtype (e.g. `html`) of a media type, or empty.",
        },
        StdItem {
            name: "charset",
            kind: StdItemKind::Function,
            doc: "Return the `charset` parameter, or empty.",
        },
        StdItem {
            name: "boundary",
            kind: StdItemKind::Function,
            doc: "Return the multipart `boundary` parameter, or empty.",
        },
        StdItem {
            name: "param",
            kind: StdItemKind::Function,
            doc: "Return an arbitrary parameter by key, or empty.",
        },
        StdItem {
            name: "type_by_extension",
            kind: StdItemKind::Function,
            doc: "Canonical media type for a filename extension (dot optional), or empty.",
        },
        StdItem {
            name: "extension_by_type",
            kind: StdItemKind::Function,
            doc: "Canonical extension (no leading dot) for a media type, or empty.",
        },
        StdItem {
            name: "is_valid",
            kind: StdItemKind::Function,
            doc: "Return true iff the string parses as a valid media type.",
        },
    ],
};
