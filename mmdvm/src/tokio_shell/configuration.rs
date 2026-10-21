//! Versioned D-STAR-only configuration for the public MMDVM protocol.
//!
//! The layouts follow `MMDVMHost/Modem.cpp:setConfig1` and `setConfig2` at
//! commit `be804801ac33f5b5d93278a2ad1507db0c269023`. They select simplex,
//! non-inverted D-STAR operation, start in Idle, disable other modes, use
//! neutral DC offsets, and leave reserved bytes zero. Callers supply their
//! measured receive level, transmit level, and key-up delay. Selecting a
//! layout establishes its encoding, not compatibility with a radio firmware.

use std::fmt;

use mmdvm_core::ProtocolVersion;

/// Configuration dialect selected before opening or changing a radio endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfigurationProfile {
    /// Public MMDVM protocol version 1, with a 23-byte configuration payload.
    MmdvmV1,
    /// Public MMDVM protocol version 2, with a 37-byte configuration payload.
    MmdvmV2,
    /// Kenwood Terminal configuration, for which no verified layout is provided.
    ///
    /// [`DstarConfiguration::new`] rejects this profile before any I/O.
    KenwoodTerminal,
}

/// Key-up delay encoded in ten-millisecond units, from zero through 500 ms.
///
/// The maximum is the check in `MMDVM/SerialPort.cpp:setConfig` at commit
/// `57e692ad8ca380891b56a0988478ca76817e9f64`. The same limit is applied as
/// host policy when producing the version 1 layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TxDelay(u8);

impl TxDelay {
    /// Construct a delay from its ten-millisecond wire units.
    ///
    /// Returns `None` above 50 units. A value of zero requests no key-up delay.
    #[must_use]
    pub const fn new(units: u8) -> Option<Self> {
        if units <= 50 { Some(Self(units)) } else { None }
    }

    /// Ten-millisecond wire units.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl From<TxDelay> for u8 {
    fn from(value: TxDelay) -> Self {
        value.0
    }
}

impl TryFrom<u8> for TxDelay {
    type Error = ConfigurationError;

    fn try_from(units: u8) -> Result<Self, Self::Error> {
        Self::new(units).ok_or(ConfigurationError::InvalidTxDelay { units })
    }
}

impl fmt::Display for TxDelay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl fmt::Binary for TxDelay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Binary::fmt(&self.0, formatter)
    }
}

impl fmt::Octal for TxDelay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Octal::fmt(&self.0, formatter)
    }
}

impl fmt::LowerHex for TxDelay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, formatter)
    }
}

impl fmt::UpperHex for TxDelay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&self.0, formatter)
    }
}

/// An eight-bit receive or transmit level in the MMDVM configuration scale.
///
/// All 256 wire values are representable; zero is the minimum and 255 is the
/// maximum. A value is a device setting rather than a PCM sample or percentage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SignalLevel(u8);

impl SignalLevel {
    /// Construct a level from its complete eight-bit wire representation.
    #[must_use]
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// Original eight-bit wire value.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl From<u8> for SignalLevel {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

impl From<SignalLevel> for u8 {
    fn from(value: SignalLevel) -> Self {
        value.0
    }
}

impl fmt::Display for SignalLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl fmt::Binary for SignalLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Binary::fmt(&self.0, formatter)
    }
}

impl fmt::Octal for SignalLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Octal::fmt(&self.0, formatter)
    }
}

impl fmt::LowerHex for SignalLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, formatter)
    }
}

impl fmt::UpperHex for SignalLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&self.0, formatter)
    }
}

/// Named receive and transmit device levels for D-STAR configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SignalLevels {
    /// Receive input level in the device's eight-bit configuration scale.
    pub receive: SignalLevel,
    /// D-STAR transmit output level in that scale.
    pub transmit: SignalLevel,
}

/// Validated D-STAR-only configuration in an explicitly selected protocol layout.
///
/// Construct this value before radio mode entry. Then pass it to
/// [`super::AsyncModem::configure_dstar`], which requires a matching confirmed
/// version and waits for the configuration ACK or NAK.
///
/// ```
/// use mmdvm::{ConfigurationProfile, DstarConfiguration, SignalLevel, SignalLevels, TxDelay};
///
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let configuration = DstarConfiguration::new(
///     ConfigurationProfile::MmdvmV2,
///     TxDelay::try_from(10)?,
///     SignalLevels { receive: SignalLevel::new(128), transmit: SignalLevel::new(128) },
/// )?;
/// assert_eq!(configuration.protocol(), mmdvm::core::ProtocolVersion::V2);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DstarConfiguration(Layout);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Layout {
    V1([u8; 23]),
    V2([u8; 37]),
}

impl DstarConfiguration {
    /// Encode a verified public layout with caller-supplied signal settings.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigurationError::UnsupportedProfile`] for
    /// [`ConfigurationProfile::KenwoodTerminal`]. No I/O occurs.
    pub const fn new(
        profile: ConfigurationProfile,
        tx_delay: TxDelay,
        levels: SignalLevels,
    ) -> Result<Self, ConfigurationError> {
        match profile {
            ConfigurationProfile::MmdvmV1 => {
                let mut payload = [0_u8; 23];
                payload[0] = 0x80;
                payload[1] = 1;
                payload[2] = tx_delay.get();
                payload[4] = levels.receive.get();
                payload[8] = 128;
                payload[9] = levels.transmit.get();
                payload[13] = 128;
                payload[14] = 128;
                Ok(Self(Layout::V1(payload)))
            }
            ConfigurationProfile::MmdvmV2 => {
                let mut payload = [0_u8; 37];
                payload[0] = 0x80;
                payload[1] = 1;
                payload[3] = tx_delay.get();
                payload[5] = 128;
                payload[6] = 128;
                payload[7] = levels.receive.get();
                payload[9] = levels.transmit.get();
                payload[28] = 128;
                Ok(Self(Layout::V2(payload)))
            }
            ConfigurationProfile::KenwoodTerminal => {
                Err(ConfigurationError::UnsupportedProfile { profile })
            }
        }
    }

    /// Protocol version required before sending this configuration.
    #[must_use]
    pub const fn protocol(self) -> ProtocolVersion {
        match self.0 {
            Layout::V1(_) => ProtocolVersion::V1,
            Layout::V2(_) => ProtocolVersion::V2,
        }
    }

    pub(crate) const fn payload(&self) -> &[u8] {
        match &self.0 {
            Layout::V1(payload) => payload,
            Layout::V2(payload) => payload,
        }
    }
}

/// Configuration rejected before any transport write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigurationError {
    /// No verified configuration layout exists for the selected profile.
    #[error("no supported D-STAR configuration layout for {profile:?}")]
    UnsupportedProfile {
        /// Rejected configuration dialect.
        profile: ConfigurationProfile,
    },
    /// Key-up delay exceeds the supported 500 ms maximum.
    #[error("TX delay {units} exceeds 50 ten-millisecond units")]
    InvalidTxDelay {
        /// Rejected ten-millisecond wire units.
        units: u8,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_layouts_have_distinct_offsets_and_complete_reserved_bytes()
    -> Result<(), Box<dyn std::error::Error>> {
        let delay = TxDelay::try_from(10)?;
        let levels = SignalLevels {
            receive: SignalLevel::new(17),
            transmit: SignalLevel::new(239),
        };
        let v1 = DstarConfiguration::new(ConfigurationProfile::MmdvmV1, delay, levels)?;
        let v2 = DstarConfiguration::new(ConfigurationProfile::MmdvmV2, delay, levels)?;
        assert_eq!(
            v1.payload(),
            &[
                0x80, 1, 10, 0, 17, 0, 0, 0, 128, 239, 0, 0, 0, 128, 128, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
            "version 1 must retain all 23 documented payload fields"
        );
        assert_eq!(
            v2.payload(),
            &[
                0x80, 1, 0, 10, 0, 128, 128, 17, 0, 239, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 128, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
            "version 2 must retain its reserved mode byte and all 37 payload fields"
        );
        assert!(
            matches!(
                DstarConfiguration::new(ConfigurationProfile::KenwoodTerminal, delay, levels,),
                Err(ConfigurationError::UnsupportedProfile {
                    profile: ConfigurationProfile::KenwoodTerminal,
                })
            ),
            "an unverified radio-specific profile cannot yield configuration bytes"
        );
        assert!(
            TxDelay::new(51).is_none(),
            "out-of-range delay fails before I/O"
        );
        Ok(())
    }
}
