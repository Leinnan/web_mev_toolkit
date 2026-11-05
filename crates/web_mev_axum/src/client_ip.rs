//! The real client address, resolved through trusted proxies.

use std::{
    convert::Infallible,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
};

use axum::{
    extract::{ConnectInfo, FromRef, FromRequestParts},
    http::{Extensions, HeaderMap, request::Parts},
};

/// Exact peer addresses allowed to supply `X-Forwarded-For`. Empty by default,
/// which ignores the header.
#[derive(Debug, Clone, Default)]
pub struct TrustedProxies(Arc<[IpAddr]>);

impl TrustedProxies {
    /// Trusts these addresses.
    #[must_use]
    pub fn new(addresses: impl IntoIterator<Item = IpAddr>) -> Self {
        Self(addresses.into_iter().collect())
    }

    /// Resolves the client of a request. A request with no `ConnectInfo` counts
    /// as loopback. That only happens in tests, because
    /// [`serve`](crate::serve::serve) always provides it.
    #[must_use]
    pub fn resolve(&self, extensions: &Extensions, headers: &HeaderMap) -> IpAddr {
        let peer = extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map_or(IpAddr::V4(Ipv4Addr::LOCALHOST), |info| info.0.ip());
        let forwarded = headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok());
        web_mev_core::client_ip(&self.0, peer, forwarded)
    }
}

/// Extractor for the resolved client address. The state must provide
/// [`TrustedProxies`] through `FromRef`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub IpAddr);

impl<S> FromRequestParts<S> for ClientIp
where
    S: Send + Sync,
    TrustedProxies: FromRef<S>,
{
    type Rejection = Infallible;

    fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(Ok(Self(
            TrustedProxies::from_ref(state).resolve(&parts.extensions, &parts.headers),
        )))
    }
}
