//! `DPlus` TCP authentication client.
//!
//! Retrieves the cached host list from `auth.dstargateway.org:20001`.
//! The configured `DPlus` session attaches this list before its UDP LINK.
//! Returns a
//! [`dstar_gateway_core::codec::dplus::HostList`] of known REF
//! reflectors cached by the auth server.

pub use client::{AuthClient, AuthError, AuthPhase, DEFAULT_AUTH_ENDPOINT};

mod client;
