//! `ClientAuthorizer` trait for incoming client authorization.
//!
//! The reflector delegates the accept/reject decision for every new
//! client link to a pluggable [`ClientAuthorizer`]. Implementors can
//! consult bans, quotas, whitelists, or any other policy before
//! letting a client into the fan-out pool.
//!
//! The default [`AllowAllAuthorizer`] accepts every request with
//! read-write access and is intended for tests and local bring-up.

use std::net::SocketAddr;

use dstar_gateway_core::session::server::ClientRejectedReason;
use dstar_gateway_core::types::{Callsign, Module, ProtocolKind};

/// Decision boundary for accepting / rejecting a new client link.
///
/// The reflector calls [`Self::authorize`] once per inbound LINK
/// attempt. Returning `Ok(AccessPolicy)` admits the client with the
/// given access; returning `Err(RejectReason)` rejects the link and
/// causes the reflector to send the protocol-appropriate NAK.
pub trait ClientAuthorizer: Send + Sync + 'static {
    /// Called when a new client attempts to link.
    ///
    /// # Errors
    ///
    /// Returns a [`RejectReason`] describing why the link was
    /// refused. The reflector converts that reason into the correct
    /// wire-level NAK for the client's protocol.
    fn authorize(&self, request: &LinkAttempt) -> Result<AccessPolicy, RejectReason>;
}

/// One link attempt observed by the reflector.
///
/// Carries the structured inputs an authorizer typically needs —
/// protocol, callsign, peer address, and requested module — without
/// leaking any wire-level details.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct LinkAttempt {
    /// Protocol the link came in on.
    pub protocol: ProtocolKind,
    /// Linking client's callsign.
    pub callsign: Callsign,
    /// Client's source address.
    pub peer: SocketAddr,
    /// Module the client wants to link to.
    pub module: Module,
}

/// Access policy granted to an accepted client.
///
/// The reflector uses this value to gate whether inbound voice from
/// the client is forwarded to other members of the module.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessPolicy {
    /// Full RX + TX.
    ReadWrite,
    /// Listen-only — client receives streams but transmissions are dropped.
    ReadOnly,
}

/// Why a link attempt was rejected.
///
/// This enum is intentionally kept protocol-agnostic; the reflector's
/// per-protocol endpoints translate each variant into the correct
/// wire-level NAK (or silent drop, for protocols without one).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum RejectReason {
    /// Reflector at capacity.
    Busy,
    /// Callsign or IP banlisted.
    Banned {
        /// Human-readable reason.
        reason: String,
    },
    /// Module is not configured on this reflector.
    UnknownModule,
    /// Per-module max client count exceeded.
    MaxClients,
    /// Custom rejection.
    Custom {
        /// Numeric code (NOT a protocol code — internal).
        code: u8,
        /// Human-readable message.
        message: String,
    },
}

impl RejectReason {
    /// Convert this reject reason into the core-level
    /// [`ClientRejectedReason`] surfaced on `ServerEvent`.
    ///
    /// Variants map one-to-one and preserve any carried strings.
    #[must_use]
    pub fn into_core_reason(self) -> ClientRejectedReason {
        match self {
            Self::Busy => ClientRejectedReason::Busy,
            Self::Banned { reason } => ClientRejectedReason::Banned { reason },
            Self::UnknownModule => ClientRejectedReason::UnknownModule,
            Self::MaxClients => ClientRejectedReason::MaxClients,
            Self::Custom { code, message } => ClientRejectedReason::Custom { code, message },
        }
    }
}

/// Authorizer that accepts every link with [`AccessPolicy::ReadWrite`].
///
/// Intended for tests and local bring-up. Production deployments
/// should plug in a policy-aware authorizer.
#[derive(Debug, Default, Clone, Copy)]
pub struct AllowAllAuthorizer;

impl ClientAuthorizer for AllowAllAuthorizer {
    fn authorize(&self, _request: &LinkAttempt) -> Result<AccessPolicy, RejectReason> {
        Ok(AccessPolicy::ReadWrite)
    }
}

/// Authorizer that rejects every link with [`RejectReason::Banned`].
///
/// Intended for tests and negative-path bring-up — verifies the
/// shell honors an authorizer rejection (no handle created, NAK on
/// the wire, [`ClientRejected`] event emitted).
///
/// [`ClientRejected`]: dstar_gateway_core::ServerEvent::ClientRejected
#[derive(Debug, Default, Clone, Copy)]
pub struct DenyAllAuthorizer;

impl ClientAuthorizer for DenyAllAuthorizer {
    fn authorize(&self, _request: &LinkAttempt) -> Result<AccessPolicy, RejectReason> {
        Err(RejectReason::Banned {
            reason: "deny-all authorizer".to_string(),
        })
    }
}

/// Authorizer that accepts every link with [`AccessPolicy::ReadOnly`].
///
/// Intended for tests that need to exercise the read-only voice
/// drop path on the shell. Production deployments should NOT use
/// this — it provides no capacity check, no banlist, and no per-peer
/// policy.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReadOnlyAuthorizer;

impl ClientAuthorizer for ReadOnlyAuthorizer {
    fn authorize(&self, _request: &LinkAttempt) -> Result<AccessPolicy, RejectReason> {
        Ok(AccessPolicy::ReadOnly)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AccessPolicy, AllowAllAuthorizer, Callsign, ClientAuthorizer, LinkAttempt, Module,
        ProtocolKind,
    };
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    const PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);

    fn sample_request() -> LinkAttempt {
        LinkAttempt {
            protocol: ProtocolKind::DExtra,
            callsign: Callsign::from_wire_bytes(*b"W1AW    "),
            peer: PEER,
            module: Module::C,
        }
    }

    #[test]
    fn allow_all_accepts_dextra_request() {
        let auth = AllowAllAuthorizer;
        let decision = auth.authorize(&sample_request());
        assert!(matches!(decision, Ok(AccessPolicy::ReadWrite)));
    }

    #[test]
    fn access_policy_variants_distinct() {
        assert_ne!(AccessPolicy::ReadWrite, AccessPolicy::ReadOnly);
    }

    #[test]
    fn link_attempt_preserves_fields() {
        let req = sample_request();
        assert_eq!(req.protocol, ProtocolKind::DExtra);
        assert_eq!(req.callsign, Callsign::from_wire_bytes(*b"W1AW    "));
        assert_eq!(req.module, Module::C);
    }
}
