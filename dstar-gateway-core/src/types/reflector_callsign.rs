//! Validated reflector callsign (REF030 / XLX307 / DCS001 / etc.).
//!
//! Stronger than a generic [`super::callsign::Callsign`]: a
//! `ReflectorCallsign` is guaranteed to start with a known protocol
//! prefix, so it carries an inferable [`super::protocol_kind::ProtocolKind`]
//! at the type level.

use super::callsign::Callsign;
use super::protocol_kind::ProtocolKind;
use super::type_error::TypeError;

/// Reflector callsign with a known protocol prefix.
///
/// # Invariants
///
/// - Wraps a [`Callsign`] whose first three bytes are one of `REF`,
///   `XRF`, `XLX`, `DCS`.
/// - The associated [`ProtocolKind`] is cached at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[doc(alias = "reflector")]
#[doc(alias = "reflector-name")]
pub struct ReflectorCallsign {
    callsign: Callsign,
    protocol: ProtocolKind,
}

impl ReflectorCallsign {
    /// Try to build a `ReflectorCallsign` from a string slice.
    ///
    /// Validates that the input parses as a [`Callsign`] AND that its
    /// first three characters identify a known protocol prefix.
    ///
    /// # Errors
    ///
    /// - [`TypeError::InvalidCallsign`] if the string fails [`Callsign::try_from_str`].
    /// - [`TypeError::InvalidReflectorCallsign`] if the prefix is not REF/XRF/XLX/DCS.
    pub fn try_from_str(s: &str) -> Result<Self, TypeError> {
        let callsign = Callsign::try_from_str(s)?;
        let protocol =
            ProtocolKind::from_reflector_prefix(s).ok_or(TypeError::InvalidReflectorCallsign {
                reason: "prefix is not REF/XRF/XLX/DCS",
            })?;
        Ok(Self { callsign, protocol })
    }

    /// The wrapped 8-byte callsign.
    #[must_use]
    pub const fn callsign(&self) -> &Callsign {
        &self.callsign
    }

    /// The protocol identified by this reflector's prefix.
    #[must_use]
    pub const fn protocol(&self) -> ProtocolKind {
        self.protocol
    }
}

impl std::fmt::Display for ReflectorCallsign {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.callsign)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn reflector_callsign_accepts_ref030() -> TestResult {
        let rc = ReflectorCallsign::try_from_str("REF030")?;
        assert_eq!(rc.protocol(), ProtocolKind::DPlus);
        assert_eq!(rc.callsign().as_str(), "REF030");
        Ok(())
    }

    #[test]
    fn reflector_callsign_accepts_xlx307() -> TestResult {
        let rc = ReflectorCallsign::try_from_str("XLX307")?;
        assert_eq!(rc.protocol(), ProtocolKind::DExtra);
        Ok(())
    }

    #[test]
    fn reflector_callsign_accepts_xrf012() -> TestResult {
        let rc = ReflectorCallsign::try_from_str("XRF012")?;
        assert_eq!(rc.protocol(), ProtocolKind::DExtra);
        Ok(())
    }

    #[test]
    fn reflector_callsign_accepts_dcs001() -> TestResult {
        let rc = ReflectorCallsign::try_from_str("DCS001")?;
        assert_eq!(rc.protocol(), ProtocolKind::Dcs);
        Ok(())
    }

    #[test]
    fn reflector_callsign_rejects_w1aw() {
        let Err(err) = ReflectorCallsign::try_from_str("W1AW") else {
            unreachable!("W1AW has no reflector prefix");
        };
        assert!(matches!(err, TypeError::InvalidReflectorCallsign { .. }));
    }

    #[test]
    fn reflector_callsign_rejects_too_long() {
        let Err(err) = ReflectorCallsign::try_from_str("REF030123456") else {
            unreachable!("9-char callsign must be rejected by Callsign layer");
        };
        assert!(matches!(err, TypeError::InvalidCallsign { .. }));
    }

    #[test]
    fn reflector_callsign_case_insensitive_prefix() -> TestResult {
        let rc = ReflectorCallsign::try_from_str("ref030")?;
        assert_eq!(rc.protocol(), ProtocolKind::DPlus);
        Ok(())
    }
}
