//! Trusted-proxy configuration and client address resolution.

use axum::http::HeaderMap;
use std::net::IpAddr;
use std::str::FromStr;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IpCidr {
    network: IpAddr,
    prefix: u8,
}

impl IpCidr {
    /// Return whether the network contains an IP address.
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.network, ip) {
            (IpAddr::V4(network), IpAddr::V4(ip)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - self.prefix)
                };
                u32::from(network) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(network), IpAddr::V6(ip)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u128::MAX << (128 - self.prefix)
                };
                u128::from(network) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

impl FromStr for IpCidr {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (address, prefix) = value.trim().split_once('/').unwrap_or((value.trim(), ""));
        let network = address
            .parse::<IpAddr>()
            .map_err(|_| format!("invalid IP address in CIDR: {value}"))?;
        let max_prefix = if network.is_ipv4() { 32 } else { 128 };
        let prefix = if prefix.is_empty() {
            max_prefix
        } else {
            prefix
                .parse::<u8>()
                .map_err(|_| format!("invalid CIDR prefix: {value}"))?
        };
        if prefix > max_prefix {
            return Err(format!("CIDR prefix is too large: {value}"));
        }
        Ok(Self { network, prefix })
    }
}

/// Parse a comma-separated list of trusted proxy networks.
pub fn parse_trusted_proxy_cidrs(value: &str) -> Result<Vec<IpCidr>, String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::parse)
        .collect()
}

/// Return whether an address belongs to a trusted proxy network.
pub fn is_trusted(ip: IpAddr, trusted: &[IpCidr]) -> bool {
    trusted.iter().any(|cidr| cidr.contains(ip))
}

/// Resolve a client address only when the immediate peer is trusted. The
/// forwarding chain is walked from right to left, stopping at the first
/// untrusted hop, which is the closest address controlled by the client.
pub fn client_ip(headers: &HeaderMap, peer: IpAddr, trusted: &[IpCidr]) -> IpAddr {
    if !is_trusted(peer, trusted) {
        return peer;
    }

    if let Some(value) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        let hops: Option<Vec<IpAddr>> = value
            .split(',')
            .map(|part| part.trim().parse::<IpAddr>().ok())
            .collect();
        if let Some(hops) = hops {
            let mut result = peer;
            for hop in hops.into_iter().rev() {
                if !is_trusted(result, trusted) {
                    break;
                }
                result = hop;
            }
            return result;
        }
    }

    headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<IpAddr>().ok())
        .unwrap_or(peer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trusted(values: &str) -> Vec<IpCidr> {
        parse_trusted_proxy_cidrs(values).unwrap()
    }

    #[test]
    fn safe_default_ignores_forwarding_headers_even_from_loopback() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "198.51.100.1".parse().unwrap());
        assert_eq!(
            client_ip(&headers, "127.0.0.1".parse().unwrap(), &[]),
            "127.0.0.1".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn walks_forwarding_chain_from_right_to_left() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "198.51.100.7, 10.1.2.3, 127.0.0.1".parse().unwrap(),
        );
        assert_eq!(
            client_ip(
                &headers,
                "127.0.0.1".parse().unwrap(),
                &trusted("127.0.0.0/8,10.0.0.0/8")
            ),
            "198.51.100.7".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn stops_at_untrusted_intermediate_hop() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "198.51.100.7, 203.0.113.9".parse().unwrap(),
        );
        assert_eq!(
            client_ip(
                &headers,
                "10.0.0.2".parse().unwrap(),
                &trusted("10.0.0.0/8")
            ),
            "203.0.113.9".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn malformed_chain_falls_back_to_peer() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "198.51.100.7, nope".parse().unwrap());
        assert_eq!(
            client_ip(
                &headers,
                "10.0.0.2".parse().unwrap(),
                &trusted("10.0.0.0/8")
            ),
            "10.0.0.2".parse::<IpAddr>().unwrap()
        );
    }
}
