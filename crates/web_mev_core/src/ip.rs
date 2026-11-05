//! Resolving the real client address behind proxies.

use std::net::IpAddr;

/// Resolves the real client IP from the connection's `peer` and the raw
/// `X-Forwarded-For` value.
///
/// The header only counts when `peer` is a trusted proxy, and then the
/// right-most untrusted address wins. Proxies append to the header, so anything
/// further left was written by the client and can be forged.
#[must_use]
pub fn client_ip(trusted: &[IpAddr], peer: IpAddr, forwarded: Option<&str>) -> IpAddr {
    if !trusted.contains(&peer) {
        return peer;
    }
    forwarded
        .and_then(|value| {
            value
                .split(',')
                .rev()
                .filter_map(|item| item.trim().parse::<IpAddr>().ok())
                .find(|ip| !trusted.contains(ip))
        })
        .unwrap_or(peer)
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::client_ip;

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    #[test]
    fn untrusted_peer_ignores_the_header() {
        assert_eq!(
            client_ip(&[ip("10.0.0.1")], ip("203.0.113.9"), Some("1.1.1.1")),
            ip("203.0.113.9")
        );
    }

    #[test]
    fn forged_left_entries_lose_to_the_right_most_untrusted() {
        let trusted = [ip("10.0.0.1"), ip("10.0.0.2")];
        assert_eq!(
            client_ip(
                &trusted,
                ip("10.0.0.1"),
                Some("6.6.6.6, 198.51.100.7, 10.0.0.2")
            ),
            ip("198.51.100.7")
        );
    }

    #[test]
    fn falls_back_to_the_peer() {
        let trusted = [ip("10.0.0.1")];
        assert_eq!(client_ip(&trusted, ip("10.0.0.1"), None), ip("10.0.0.1"));
        assert_eq!(
            client_ip(&trusted, ip("10.0.0.1"), Some("garbage")),
            ip("10.0.0.1")
        );
        assert_eq!(
            client_ip(&trusted, ip("10.0.0.1"), Some("10.0.0.1")),
            ip("10.0.0.1")
        );
    }

    #[test]
    fn handles_ipv6_entries() {
        assert_eq!(
            client_ip(&[ip("::1")], ip("::1"), Some("2001:db8::5")),
            ip("2001:db8::5")
        );
    }
}
