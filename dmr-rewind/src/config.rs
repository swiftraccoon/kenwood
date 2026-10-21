//! Construction-validated operator IDs and outbound subscription kinds.

use std::fmt;
use std::time::Duration;

use dmr_rewind_core::{DmrId, SessionType, WireDmrId};

/// An invalid Open DMR Terminal setting, rejected before opening a socket.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// The remote UDP port was zero.
    #[error("server port must be greater than zero")]
    ZeroServerPort,
    /// A subscriber ID was outside the seven-digit service allocation range.
    #[error("dmr_id must be a seven-digit RadioID allocation; received {value}")]
    SubscriberId {
        /// Rejected numeric ID.
        value: u32,
    },
    /// A destination was zero or exceeded the 24-bit destination policy.
    #[error("subscription destination {value} is outside the nonzero 24-bit DMR ID range")]
    DestinationId {
        /// Rejected numeric ID.
        value: u32,
    },
    /// A receive-wire extension was supplied as an outbound subscription kind.
    #[error("subscription session type {raw} must be group or private voice")]
    SubscriptionKind {
        /// Rejected wire session code.
        raw: u32,
    },
    /// The password contained no bytes.
    #[error("hotspot-security password must not be empty")]
    EmptyPassword,
    /// The description contained no bytes.
    #[error("client description must not be empty")]
    EmptyDescription,
    /// A description exceeded the packet capacity after its fixed prefix.
    #[error("client description is {actual} bytes; maximum is {maximum}")]
    DescriptionTooLong {
        /// Supplied UTF-8 byte count.
        actual: usize,
        /// Maximum allowed UTF-8 byte count.
        maximum: usize,
    },
    /// The valid-packet timeout did not exceed a positive keepalive interval.
    #[error("timeout {timeout:?} must exceed the positive keepalive interval {keepalive:?}")]
    TimerRelationship {
        /// Proposed valid-packet timeout.
        timeout: Duration,
        /// Proposed keepalive interval.
        keepalive: Duration,
    },
    /// A send budget was zero.
    #[error("send timeout must be greater than zero")]
    ZeroSendTimeout,
    /// A configured duration exceeded the runtime clock's representation.
    #[error("duration {duration:?} is too large for the runtime clock")]
    ClockRange {
        /// Rejected interval.
        duration: Duration,
    },
    /// A subscription list contained the same kind and destination twice.
    #[error("duplicate {kind:?} subscription for {destination}")]
    DuplicateSubscription {
        /// Repeated voice-session kind.
        kind: SubscriptionKind,
        /// Repeated destination.
        destination: DestinationId,
    },
    /// Local and remote endpoints used different IP families.
    #[error("local bind and server must use the same IP family")]
    AddressFamily,
}

/// A seven-digit operator subscriber allocation, from 1000000 through 9999999.
///
/// This outbound service policy is separate from received full-width wire IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubscriberId(u32);

impl SubscriberId {
    /// Validate the seven-digit service allocation range.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::SubscriberId`] outside 1000000 through 9999999.
    pub const fn new(value: u32) -> Result<Self, ConfigError> {
        if value >= 1_000_000 && value <= 9_999_999 {
            Ok(Self(value))
        } else {
            Err(ConfigError::SubscriberId { value })
        }
    }

    /// Return the decimal subscriber ID.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for SubscriberId {
    type Error = ConfigError;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SubscriberId> for u32 {
    fn from(value: SubscriberId) -> Self {
        value.get()
    }
}

impl From<SubscriberId> for WireDmrId {
    fn from(value: SubscriberId) -> Self {
        Self::new(value.get())
    }
}

impl fmt::Display for SubscriberId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// A nonzero outbound group/private destination fitting 24 bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DestinationId(DmrId);

impl DestinationId {
    /// Validate a destination in 1 through 16777215.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::DestinationId`] for zero or a value above 24 bits.
    pub const fn new(value: u32) -> Result<Self, ConfigError> {
        if value == 0 {
            return Err(ConfigError::DestinationId { value });
        }
        match DmrId::new(value) {
            Ok(value) => Ok(Self(value)),
            Err(_) => Err(ConfigError::DestinationId { value }),
        }
    }

    /// Return the decimal destination ID.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl TryFrom<u32> for DestinationId {
    type Error = ConfigError;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<DestinationId> for u32 {
    fn from(value: DestinationId) -> Self {
        value.get()
    }
}

impl From<DestinationId> for WireDmrId {
    fn from(value: DestinationId) -> Self {
        Self::new(value.get())
    }
}

impl From<DestinationId> for DmrId {
    fn from(value: DestinationId) -> Self {
        value.0
    }
}

impl fmt::Display for DestinationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// The two outbound voice-session kinds accepted by Open DMR Terminal subscriptions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SubscriptionKind {
    /// Subscribe to group voice calls.
    Group,
    /// Subscribe to private voice calls.
    Private,
}

impl From<SubscriptionKind> for SessionType {
    fn from(value: SubscriptionKind) -> Self {
        match value {
            SubscriptionKind::Group => Self::GroupVoice,
            SubscriptionKind::Private => Self::PrivateVoice,
        }
    }
}

impl TryFrom<SessionType> for SubscriptionKind {
    type Error = ConfigError;
    /// Convert a named voice-session kind, rejecting unmodeled wire extensions.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::SubscriptionKind`] for an unknown session code.
    fn try_from(value: SessionType) -> Result<Self, Self::Error> {
        match value {
            SessionType::GroupVoice => Ok(Self::Group),
            SessionType::PrivateVoice => Ok(Self::Private),
            SessionType::Unknown(value) => Err(ConfigError::SubscriptionKind { raw: value.get() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientConfig, Subscription};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn configuration() -> Result<ClientConfig, ConfigError> {
        ClientConfig::new(
            std::net::SocketAddr::from(([127, 0, 0, 1], 54006)),
            SubscriberId::new(1_234_567)?,
            "fixture-password".to_owned(),
        )
    }

    #[test]
    fn subscriber_and_destination_domains_reject_invalid_construction() -> TestResult {
        for value in [1_000_000, 9_999_999] {
            assert_eq!(SubscriberId::new(value)?.get(), value);
        }
        for value in [0, 999_999, 10_000_000, u32::MAX] {
            assert!(
                matches!(SubscriberId::new(value), Err(ConfigError::SubscriberId { value: rejected }) if rejected == value)
            );
        }
        for value in [1, 0x00ff_ffff] {
            assert_eq!(DestinationId::new(value)?.get(), value);
        }
        for value in [0, 0x0100_0000, u32::MAX] {
            assert!(
                matches!(DestinationId::new(value), Err(ConfigError::DestinationId { value: rejected }) if rejected == value)
            );
        }
        assert_eq!(
            SubscriptionKind::try_from(SessionType::GroupVoice)?,
            SubscriptionKind::Group
        );
        assert!(matches!(
            SubscriptionKind::try_from(SessionType::from_raw(0xffff_ffff)),
            Err(ConfigError::SubscriptionKind { raw: u32::MAX })
        ));
        Ok(())
    }

    #[test]
    fn every_configuration_boundary_preserves_complete_validation() -> TestResult {
        let base = configuration()?;
        assert!(matches!(
            base.clone().with_description(String::new()),
            Err(ConfigError::EmptyDescription)
        ));
        assert!(matches!(
            base.clone()
                .with_description("x".repeat(dmr_rewind_core::MAX_PAYLOAD_LEN)),
            Err(ConfigError::DescriptionTooLong { .. })
        ));
        assert!(matches!(
            base.clone().with_timeout(Duration::from_secs(5)),
            Err(ConfigError::TimerRelationship { .. })
        ));
        assert!(matches!(
            base.clone().with_keepalive_interval(Duration::ZERO),
            Err(ConfigError::TimerRelationship { .. })
        ));
        assert!(matches!(
            base.clone().with_timeout(Duration::MAX),
            Err(ConfigError::ClockRange { .. })
        ));
        assert!(matches!(
            base.clone().with_send_timeout(Duration::ZERO),
            Err(ConfigError::ZeroSendTimeout)
        ));
        assert!(matches!(
            base.clone().with_send_timeout(Duration::MAX),
            Err(ConfigError::ClockRange { .. })
        ));
        assert!(matches!(
            base.clone()
                .with_bind(std::net::SocketAddr::from(([0_u16; 8], 0))),
            Err(ConfigError::AddressFamily)
        ));
        let group = Subscription::group(DestinationId::new(91)?);
        assert!(matches!(
            base.clone().with_subscriptions(vec![group, group]),
            Err(ConfigError::DuplicateSubscription { .. })
        ));
        let subscriptions = vec![group, Subscription::private(DestinationId::new(91)?)];
        let configured = base.with_subscriptions(subscriptions.clone())?;
        assert_eq!(configured.subscriptions(), subscriptions);
        assert_eq!(configured.dmr_id().get(), 1_234_567);
        Ok(())
    }

    #[test]
    fn invalid_initial_configuration_never_becomes_usable() -> TestResult {
        let id = SubscriberId::new(1_234_567)?;
        let endpoint = std::net::SocketAddr::from(([127, 0, 0, 1], 54006));
        assert!(matches!(
            ClientConfig::new(endpoint, id, String::new()),
            Err(ConfigError::EmptyPassword)
        ));
        assert!(matches!(
            ClientConfig::new(
                std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
                id,
                "fixture-password".to_owned()
            ),
            Err(ConfigError::ZeroServerPort)
        ));
        Ok(())
    }
}
