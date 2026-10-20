//! Which addresses the gateway may connect to on the web: public unicast
//! addresses only.
//!
//! An address is public unless it falls in a range that IANA's special-
//! purpose registries or the household's own networks reserve: loopback,
//! the private ranges of RFC 1918, link-local, the shared address space
//! 100.64.0.0/10 (which the tailnet uses), unique local IPv6, multicast,
//! unspecified, documentation, benchmarking and the other reserved ranges.
//! IPv6 is allowed only within global unicast (2000::/3), less its
//! reserved parts; an IPv6 address that carries an IPv4 address (mapped,
//! or the NAT64 well-known prefix) is judged by that IPv4 address.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Whether the address is public: one the gateway may connect to on the
/// web's behalf.
pub fn is_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

/// IPv4 ranges that are not public, as (network, prefix length).
const NON_PUBLIC_V4: &[([u8; 4], u8)] = &[
    ([0, 0, 0, 0], 8),       // "this network", including unspecified
    ([10, 0, 0, 0], 8),      // private (RFC 1918)
    ([100, 64, 0, 0], 10),   // shared address space, used by the tailnet
    ([127, 0, 0, 0], 8),     // loopback
    ([169, 254, 0, 0], 16),  // link-local
    ([172, 16, 0, 0], 12),   // private (RFC 1918)
    ([192, 0, 0, 0], 24),    // IETF protocol assignments
    ([192, 0, 2, 0], 24),    // documentation (TEST-NET-1)
    ([192, 31, 196, 0], 24), // AS112-v4
    ([192, 52, 193, 0], 24), // AMT
    ([192, 88, 99, 0], 24),  // deprecated 6to4 relay anycast
    ([192, 168, 0, 0], 16),  // private (RFC 1918)
    ([192, 175, 48, 0], 24), // direct delegation AS112 service
    ([198, 18, 0, 0], 15),   // benchmarking
    ([198, 51, 100, 0], 24), // documentation (TEST-NET-2)
    ([203, 0, 113, 0], 24),  // documentation (TEST-NET-3)
    ([224, 0, 0, 0], 4),     // multicast
    ([240, 0, 0, 0], 4),     // reserved, including broadcast
];

fn is_public_v4(address: Ipv4Addr) -> bool {
    let bits = u32::from(address);
    !NON_PUBLIC_V4.iter().any(|(network, prefix)| {
        let mask = u32::MAX << (32 - prefix);
        bits & mask == u32::from_be_bytes(*network) & mask
    })
}

/// Reserved ranges within global unicast (2000::/3), as (network, prefix
/// length).
const NON_PUBLIC_V6: &[(u128, u8)] = &[
    (0x2001_0000 << 96, 23), // IETF protocol assignments, including Teredo
    (0x2001_0db8 << 96, 32), // documentation
    (0x2002 << 112, 16),     // 6to4, which carries IPv4 addresses
    (0x3fff << 112, 20),     // documentation
];

/// The NAT64 well-known prefix, 64:ff9b::/96.
const NAT64: u128 = 0x0064_ff9b << 96;

fn in_range(bits: u128, network: u128, prefix: u8) -> bool {
    let mask = u128::MAX << (128 - prefix);
    bits & mask == network & mask
}

fn is_public_v6(address: Ipv6Addr) -> bool {
    if let Some(v4) = address.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let bits = u128::from(address);
    if in_range(bits, NAT64, 96) {
        return is_public_v4(Ipv4Addr::from(bits as u32));
    }
    // Everything outside global unicast is not public: unspecified,
    // loopback, IPv4-compatible, discard-only, unique local, link-local,
    // site-local and multicast among them.
    in_range(bits, 0x2000 << 112, 3)
        && !NON_PUBLIC_V6
            .iter()
            .any(|(network, prefix)| in_range(bits, *network, *prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public(text: &str) -> bool {
        is_public(text.parse().unwrap())
    }

    #[test]
    fn public_ipv4_addresses_are_allowed() {
        for address in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34",
            "9.255.255.255",
            "11.0.0.0",
            "100.63.255.255",
            "100.128.0.0",
            "126.255.255.255",
            "128.0.0.0",
            "169.253.255.255",
            "172.15.255.255",
            "172.32.0.0",
            "192.0.3.0",
            "192.167.255.255",
            "192.169.0.0",
            "198.17.255.255",
            "198.20.0.0",
            "223.255.255.255",
        ] {
            assert!(public(address), "{address}");
        }
    }

    #[test]
    fn non_public_ipv4_addresses_are_refused() {
        for address in [
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "10.255.255.255",
            "100.64.0.1",
            "100.100.100.100",
            "100.127.255.255",
            "127.0.0.1",
            "127.255.255.254",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.31.196.1",
            "192.52.193.1",
            "192.88.99.1",
            "192.168.0.1",
            "192.168.50.10",
            "192.175.48.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            assert!(!public(address), "{address}");
        }
    }

    #[test]
    fn public_ipv6_addresses_are_allowed() {
        for address in [
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
            "2a00:1450:4001:80b::200e",
            "2001:200::1",
            "3ffe::1",
            "2003::1",
        ] {
            assert!(public(address), "{address}");
        }
    }

    #[test]
    fn non_public_ipv6_addresses_are_refused() {
        for address in [
            "::",
            "::1",
            "::127.0.0.1",
            "::8.8.8.8",
            "100::1",
            "64:ff9b:1::1",
            "2001::1",
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
            "2001:2::1",
            "2001:10::1",
            "2001:db8::1",
            "2002:c000:0204::1",
            "2002:0808:0808::1",
            "3fff::1",
            "3fff:fff::1",
            "fc00::1",
            "fd12:3456:789a::1",
            "fe80::1",
            "febf::1",
            "fec0::1",
            "ff02::1",
            "ff0e::1",
            "4000::1",
            "5f00::1",
            "8000::1",
            "e000::1",
        ] {
            assert!(!public(address), "{address}");
        }
    }

    #[test]
    fn ipv6_addresses_carrying_ipv4_are_judged_by_it() {
        for refused in [
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:192.168.1.1",
            "::ffff:169.254.169.254",
            "::ffff:100.64.0.1",
            "::ffff:0.0.0.0",
            "64:ff9b::127.0.0.1",
            "64:ff9b::10.1.2.3",
            "64:ff9b::192.168.0.1",
        ] {
            assert!(!public(refused), "{refused}");
        }
        for allowed in ["::ffff:8.8.8.8", "64:ff9b::1.1.1.1"] {
            assert!(public(allowed), "{allowed}");
        }
    }
}
