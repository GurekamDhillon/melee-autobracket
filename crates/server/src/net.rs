//! Who a request comes from: the TCP peer, or behind a trusted reverse proxy the client the
//! proxy names in `X-Forwarded-For`. Rate limiting keys on this address, IPv6 by /64.

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use anyhow::bail;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request};
use tower_governor::key_extractor::KeyExtractor;
use tower_governor::GovernorError;

/// An IP network: an address and a prefix length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    net: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// `ADDR/PREFIX`, or a bare address (a single host).
    pub fn parse(s: &str) -> Option<Cidr> {
        let (addr, prefix) = match s.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (s, None),
        };
        let net = canonical(addr.parse().ok()?);
        let max = max_prefix(net);
        let prefix = match prefix {
            None => max,
            Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => p.parse().ok()?,
            Some(_) => return None,
        };
        (prefix <= max).then_some(Cidr { net, prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.net, canonical(ip)) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                top_bits(u128::from(u32::from(net)), self.prefix, 32)
                    == top_bits(u128::from(u32::from(ip)), self.prefix, 32)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                top_bits(u128::from(net), self.prefix, 128)
                    == top_bits(u128::from(ip), self.prefix, 128)
            }
            _ => false,
        }
    }
}

fn max_prefix(ip: IpAddr) -> u8 {
    match ip {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    }
}

/// The top `prefix` bits of a `width`-bit value (`prefix <= width`).
fn top_bits(v: u128, prefix: u8, width: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        v >> (width - prefix)
    }
}

/// An IPv4-mapped IPv6 address as plain IPv4; anything else unchanged.
fn canonical(ip: IpAddr) -> IpAddr {
    ip.to_canonical()
}

/// Parses every entry; the error names the first that is not a CIDR.
pub fn parse_cidrs(entries: &[String]) -> anyhow::Result<Vec<Cidr>> {
    entries
        .iter()
        .map(|e| match Cidr::parse(e) {
            Some(c) => Ok(c),
            None => bail!("{e:?} is not an IP network (ADDR/PREFIX) or address"),
        })
        .collect()
}

/// The client a request comes from. When the TCP `peer` is a trusted proxy, the right-most
/// `X-Forwarded-For` entry that is not itself a trusted proxy (every proxy appends the
/// address it received the request from, so entries to the left of that one are the client's
/// own claims); otherwise, or when the header names no such address, the peer.
pub fn client_ip(peer: IpAddr, headers: &HeaderMap, trusted: &[Cidr]) -> IpAddr {
    let peer = canonical(peer);
    let is_trusted = |ip: IpAddr| trusted.iter().any(|c| c.contains(ip));
    if !is_trusted(peer) {
        return peer;
    }
    // Repeated header lines form one list, in order.
    let entries: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .collect();
    for entry in entries.iter().rev() {
        let Some(ip) = parse_forwarded(entry.trim()) else {
            // Not written by a proxy: trust nothing further left.
            return peer;
        };
        if !is_trusted(ip) {
            return ip;
        }
    }
    peer
}

/// An `X-Forwarded-For` entry: an address, possibly with a port (`1.2.3.4:5`, `[::1]:5`).
fn parse_forwarded(entry: &str) -> Option<IpAddr> {
    entry
        .parse::<IpAddr>()
        .or_else(|_| entry.parse::<SocketAddr>().map(|s| s.ip()))
        .ok()
        .map(canonical)
}

/// The rate-limit bucket of a client: its IPv4 address, or the /64 of its IPv6 address (one
/// subscriber usually holds a whole /64 and can pick any address in it).
pub fn rate_key(ip: IpAddr) -> IpAddr {
    match canonical(ip) {
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & !(u128::from(u64::MAX)))),
        v4 => v4,
    }
}

/// Rate-limit key extractor: [`rate_key`] of [`client_ip`].
#[derive(Clone, Debug)]
pub struct ClientKey {
    pub trusted: Arc<[Cidr]>,
}

impl KeyExtractor for ClientKey {
    type Key = IpAddr;

    fn extract<T>(&self, req: &Request<T>) -> Result<IpAddr, GovernorError> {
        let peer = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip())
            .ok_or(GovernorError::UnableToExtractKey)?;
        Ok(rate_key(client_ip(peer, req.headers(), &self.trusted)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn xff(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append("x-forwarded-for", v.parse().unwrap());
        }
        h
    }

    #[test]
    fn cidr_parse_and_contains() {
        let docker = Cidr::parse("172.16.0.0/12").unwrap();
        assert!(docker.contains(ip("172.17.0.1")));
        assert!(docker.contains(ip("172.31.255.255")));
        assert!(!docker.contains(ip("172.32.0.1")));
        assert!(docker.contains(ip("::ffff:172.17.0.1")), "IPv4-mapped");
        assert!(!docker.contains(ip("2001:db8::1")));

        let host = Cidr::parse("10.0.0.1").unwrap();
        assert!(host.contains(ip("10.0.0.1")));
        assert!(!host.contains(ip("10.0.0.2")));

        assert!(Cidr::parse("0.0.0.0/0").unwrap().contains(ip("8.8.8.8")));
        let v6 = Cidr::parse("2001:db8::/32").unwrap();
        assert!(v6.contains(ip("2001:db8:ffff::1")));
        assert!(!v6.contains(ip("2001:db9::1")));
        assert!(Cidr::parse("::1/128").unwrap().contains(ip("::1")));

        for bad in [
            "",
            "/8",
            "10.0.0.0/",
            "10.0.0.0/33",
            "::/129",
            "10.0.0.0/+8",
            "x/8",
            "1.2.3.4/-1",
        ] {
            assert_eq!(Cidr::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn forwarded_for_only_from_a_trusted_peer() {
        let trusted = [Cidr::parse("172.16.0.0/12").unwrap()];
        let proxy = ip("172.18.0.2");
        // Behind the proxy: the address the proxy appended.
        assert_eq!(
            client_ip(proxy, &xff(&["203.0.113.7"]), &trusted),
            ip("203.0.113.7")
        );
        // A client's own claim to the left is ignored.
        assert_eq!(
            client_ip(proxy, &xff(&["1.1.1.1, 203.0.113.7"]), &trusted),
            ip("203.0.113.7")
        );
        // Repeated header lines are one list.
        assert_eq!(
            client_ip(proxy, &xff(&["1.1.1.1", "203.0.113.7"]), &trusted),
            ip("203.0.113.7")
        );
        // A chain of trusted proxies is skipped.
        assert_eq!(
            client_ip(proxy, &xff(&["203.0.113.7, 172.20.0.5"]), &trusted),
            ip("203.0.113.7")
        );
        // Ports are dropped.
        assert_eq!(
            client_ip(proxy, &xff(&["[2001:db8::1]:443"]), &trusted),
            ip("2001:db8::1")
        );
        // No header, garbage, or only trusted entries: the peer.
        assert_eq!(client_ip(proxy, &HeaderMap::new(), &trusted), proxy);
        assert_eq!(client_ip(proxy, &xff(&["1.1.1.1, junk"]), &trusted), proxy);
        assert_eq!(client_ip(proxy, &xff(&["172.20.0.5"]), &trusted), proxy);

        // From an untrusted peer the header is ignored.
        let outsider = ip("198.51.100.9");
        assert_eq!(
            client_ip(outsider, &xff(&["203.0.113.7"]), &trusted),
            outsider
        );
        assert_eq!(client_ip(outsider, &xff(&["203.0.113.7"]), &[]), outsider);
    }

    /// Why the docs say to trust only the proxy's own address: a trusted range that also
    /// holds a client's real address lets that client pick its address with a forged
    /// left-most `X-Forwarded-For` entry.
    #[test]
    fn a_trusted_range_holding_clients_lets_them_choose_their_address() {
        let gateway = ip("10.203.47.1");
        let lan_client = ip("192.168.1.50");
        // nginx appends the address it received the request from to what the client sent.
        let headers = xff(&["6.6.6.6, 192.168.1.50"]);

        let broad = [
            Cidr::parse("172.16.0.0/12").unwrap(),
            Cidr::parse("192.168.0.0/16").unwrap(),
            Cidr::parse("10.203.47.1/32").unwrap(),
        ];
        assert_eq!(client_ip(gateway, &headers, &broad), ip("6.6.6.6"));

        let narrow = [Cidr::parse("10.203.47.1/32").unwrap()];
        assert_eq!(client_ip(gateway, &headers, &narrow), lan_client);
        // And a LAN peer connecting directly is not taken for the proxy.
        assert_eq!(client_ip(lan_client, &headers, &narrow), lan_client);
    }

    #[test]
    fn ipv6_clients_share_a_slash_64() {
        assert_eq!(rate_key(ip("2001:db8:1:2:aaaa::1")), ip("2001:db8:1:2::"));
        assert_eq!(rate_key(ip("2001:db8:1:2:bbbb::9")), ip("2001:db8:1:2::"));
        assert_ne!(rate_key(ip("2001:db8:1:3::1")), ip("2001:db8:1:2::"));
        assert_eq!(rate_key(ip("192.0.2.1")), ip("192.0.2.1"));
        assert_eq!(rate_key(ip("::ffff:192.0.2.1")), ip("192.0.2.1"));
    }
}
