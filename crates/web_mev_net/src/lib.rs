//! Outbound HTTP for URLs that users can influence, such as feeds and webhooks.
//!
//! Every connection resolves the destination itself and only connects to globally
//! routable addresses, which blocks loopback, private networks, cloud metadata and
//! DNS rebinding. Redirects are followed by hand, so each hop is checked again, and
//! bodies are read with a size limit.

pub mod fetch;
pub mod policy;

pub use fetch::{
    FetchError, FetchOptions, Fetched, Outcome, SafeClient, Validators, parse_http_url,
};
pub use policy::{
    BoxResolveFuture, PolicyError, PolicyResolver, Resolver, SystemResolver, find_policy_error,
    is_allowed_destination, resolve_allowed_with,
};
