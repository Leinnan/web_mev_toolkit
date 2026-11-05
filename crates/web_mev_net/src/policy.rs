//! Which destinations the server may contact.

use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    sync::Arc,
};

/// Failure to resolve or validate a destination.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    /// The hostname didn't resolve.
    #[error("host {0} did not resolve")]
    Resolution(String),
    /// The host resolved, but no address may be contacted.
    #[error("host resolves only to addresses outside the public internet")]
    Blocked,
}

/// Returns true when the address is safe to contact from the server.
#[must_use]
pub fn is_allowed_destination(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_allowed_v4(v4),
        IpAddr::V6(v6) => is_allowed_v6(v6),
    }
}

fn is_allowed_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(
        a == 0                        // "this network"
        || a == 10                    // RFC 1918 private
        || a == 127                   // loopback
        || (a == 100 && (64..=127).contains(&b)) // carrier-grade NAT
        || (a == 169 && b == 254)     // link-local, cloud metadata
        || (a == 172 && (16..=31).contains(&b)) // RFC 1918 private
        || (a == 192 && b == 168)     // RFC 1918 private
        || (a == 192 && b == 0 && c == 0) // IETF protocol assignments
        || (a == 192 && b == 0 && c == 2) // TEST-NET-1 documentation
        || (a == 192 && b == 88 && c == 99) // 6to4 relay anycast
        || (a == 198 && (b == 18 || b == 19)) // inter-network benchmark
        || (a == 198 && b == 51 && c == 100) // TEST-NET-2 documentation
        || (a == 203 && b == 0 && c == 113) // TEST-NET-3 documentation
        || a >= 224
        // multicast, reserved, broadcast
    )
}

fn is_allowed_v6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    if ip.is_loopback() || ip.is_unspecified() {
        return false;
    }
    // ::ffff:0:0/96: judge the embedded IPv4 address instead.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_allowed_v4(v4);
    }
    !(
        // These prefixes all tunnel to an IPv4 address the check above never sees,
        // so an attacker could use them to reach a private one.
        (segments[0] == 0x0064 && segments[1] == 0xff9b) // NAT64 64:ff9b::/32
        || segments[0] == 0x2002 // 6to4 2002::/16
        || (segments[0] == 0x2001 && segments[1] < 0x0200) // Teredo and IETF 2001::/23
        || (segments[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
        || (segments[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        || (segments[0] & 0xff00) == 0xff00 // multicast ff00::/8
        || (segments[0] == 0x2001 && segments[1] == 0x0db8) // documentation
        || (segments[0] & 0xe000) != 0x2000
        // require global unicast 2000::/3
    )
}

/// The future a [`Resolver`] returns.
pub type BoxResolveFuture = Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>>;

/// Resolves hostnames. It's a trait so tests never touch real DNS.
pub trait Resolver: Send + Sync + 'static {
    /// Every address `host` resolves to, unfiltered. The policy filters separately.
    fn resolve(&self, host: String, port: u16) -> BoxResolveFuture;
}

/// The system DNS stack.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve(&self, host: String, port: u16) -> BoxResolveFuture {
        Box::pin(async move {
            let addresses = tokio::net::lookup_host((host.as_str(), port))
                .await?
                .map(|address| address.ip())
                .collect();
            Ok(addresses)
        })
    }
}

/// Resolves a destination and keeps only the addresses the policy allows.
///
/// # Errors
///
/// Returns [`PolicyError::Resolution`] when the lookup fails, and
/// [`PolicyError::Blocked`] when no address may be contacted.
pub async fn resolve_allowed_with<F, Fut>(
    host: &str,
    port: u16,
    resolve: F,
) -> Result<Vec<SocketAddr>, PolicyError>
where
    F: FnOnce(String, u16) -> Fut,
    Fut: Future<Output = std::io::Result<Vec<IpAddr>>>,
{
    let resolved = resolve(host.to_owned(), port)
        .await
        .map_err(|_| PolicyError::Resolution(host.to_owned()))?;
    let allowed: Vec<SocketAddr> = resolved
        .into_iter()
        .filter(|ip| is_allowed_destination(*ip))
        .map(|ip| SocketAddr::new(ip, port))
        .collect();
    if allowed.is_empty() {
        return Err(PolicyError::Blocked);
    }
    Ok(allowed)
}

/// The DNS step of a reqwest client. It hands the connector only addresses the
/// policy allows, so every connection, pooled or new, first hop or redirect, is
/// checked at the moment it is made.
pub struct PolicyResolver {
    inner: Arc<dyn Resolver>,
    allow_private: bool,
}

impl PolicyResolver {
    /// Wraps `inner`. `allow_private` turns the filter off, for operators fetching
    /// from their own network.
    #[must_use]
    pub fn new(inner: Arc<dyn Resolver>, allow_private: bool) -> Self {
        Self {
            inner,
            allow_private,
        }
    }
}

impl reqwest::dns::Resolve for PolicyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let inner = self.inner.clone();
        let allow_private = self.allow_private;
        let host = name.as_str().to_owned();
        Box::pin(async move {
            // Port 0: the connector fills in the URL's port.
            let resolved = inner
                .resolve(host.clone(), 0)
                .await
                .map_err(|_| PolicyError::Resolution(host))?;
            let allowed: Vec<SocketAddr> = resolved
                .into_iter()
                .filter(|ip| allow_private || is_allowed_destination(*ip))
                .map(|ip| SocketAddr::new(ip, 0))
                .collect();
            if allowed.is_empty() {
                return Err(PolicyError::Blocked.into());
            }
            let addresses: reqwest::dns::Addrs = Box::new(allowed.into_iter());
            Ok(addresses)
        })
    }
}

/// Finds a [`PolicyError`] raised by [`PolicyResolver`] in an error's source chain.
#[must_use]
pub fn find_policy_error<'e>(
    error: &'e (dyn std::error::Error + 'static),
) -> Option<&'e PolicyError> {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(policy) = error.downcast_ref::<PolicyError>() {
            return Some(policy);
        }
        current = error.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::{PolicyError, is_allowed_destination, resolve_allowed_with};

    fn allowed(input: &str) -> bool {
        is_allowed_destination(input.parse().unwrap())
    }

    #[test]
    fn blocked_ranges_are_rejected() {
        for ip in [
            "127.0.0.1",
            "127.8.8.8",
            "10.0.0.5",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "198.18.0.1",
            "192.0.2.1",
            "192.88.99.1",
            "198.51.100.7",
            "203.0.113.9",
            "::1",
            "::",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
        ] {
            assert!(!allowed(ip), "{ip} must be blocked");
        }
    }

    /// The copies in zbierak and `oxi_feed` read the wrong segments for NAT64, and let
    /// 6to4 and Teredo through.
    #[test]
    fn tunnels_to_ipv4_are_rejected() {
        for ip in [
            // NAT64 to 127.0.0.1 and 169.254.169.254.
            "64:ff9b::7f00:1",
            "64:ff9b::a9fe:a9fe",
            // NAT64 local-use prefix.
            "64:ff9b:1::1",
            // 6to4 to 127.0.0.1 and 10.0.0.1.
            "2002:7f00:1::",
            "2002:a00:1::",
            // Teredo.
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
        ] {
            assert!(!allowed(ip), "{ip} must be blocked");
        }
    }

    #[test]
    fn public_addresses_are_accepted() {
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "172.15.255.255",
            "2606:4700::1111",
            "2001:4860:4860::8888",
            "::ffff:8.8.8.8",
        ] {
            assert!(allowed(ip), "{ip} must be allowed");
        }
    }

    #[tokio::test]
    async fn resolver_output_is_filtered_and_blocking_is_reported() {
        let mixed = |host: String, _port: u16| {
            assert_eq!(host, "example.test");
            async {
                Ok(vec![
                    "10.0.0.1".parse::<IpAddr>().unwrap(),
                    "93.184.216.34".parse::<IpAddr>().unwrap(),
                ])
            }
        };
        let pinned = resolve_allowed_with("example.test", 443, mixed)
            .await
            .unwrap();
        assert_eq!(pinned, vec!["93.184.216.34:443".parse().unwrap()]);

        let internal_only = |_host: String, _port: u16| async {
            Ok(vec![
                "127.0.0.1".parse::<IpAddr>().unwrap(),
                "192.168.0.2".parse::<IpAddr>().unwrap(),
            ])
        };
        let error = resolve_allowed_with("example.test", 443, internal_only)
            .await
            .unwrap_err();
        assert_eq!(error, PolicyError::Blocked);

        let failing =
            |_host: String, _port: u16| async { Err(std::io::Error::other("no such host")) };
        let error = resolve_allowed_with("example.test", 443, failing)
            .await
            .unwrap_err();
        assert_eq!(error, PolicyError::Resolution("example.test".into()));
    }
}
