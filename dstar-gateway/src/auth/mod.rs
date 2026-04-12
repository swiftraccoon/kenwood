//! `DPlus` TCP authentication client.
//!
//! Performs the mandatory TCP auth step with
//! `auth.dstargateway.org:20001` before a `DPlus` (REF) reflector will
//! accept UDP packets from the callsign. Returns a
//! [`dstar_gateway_core::codec::dplus::HostList`] of known REF
//! reflectors cached by the auth server.

mod client;

pub use client::{AuthClient, AuthError, AuthPhase, DEFAULT_AUTH_ENDPOINT};
