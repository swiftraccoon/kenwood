//! Validated TH-D75 KISS TNC control parameters.
//!
//! KISS encodes timing controls as one byte in ten-millisecond units. These
//! types keep that wire detail out of public radio APIs, enforce the ranges
//! accepted by the TH-D75, and make call sites state real durations.

use std::fmt;

use crate::error::ValidationError;

const MILLISECONDS_PER_WIRE_UNIT: u16 = 10;

const fn encode_timing(milliseconds: u16, maximum_milliseconds: u16) -> Option<u8> {
    if milliseconds > maximum_milliseconds
        || !milliseconds.is_multiple_of(MILLISECONDS_PER_WIRE_UNIT)
    {
        return None;
    }

    let [wire_value, overflow] = (milliseconds / MILLISECONDS_PER_WIRE_UNIT).to_le_bytes();
    match overflow {
        0 => Some(wire_value),
        _ => None,
    }
}

const fn decode_timing(wire_value: u8) -> u16 {
    u16::from_le_bytes([wire_value, 0]) * MILLISECONDS_PER_WIRE_UNIT
}

/// KISS transmitter key-up delay for the TH-D75.
///
/// The radio accepts 0 through 1200 milliseconds in 10 millisecond steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KissTxDelay(u8);

impl KissTxDelay {
    /// Maximum accepted delay in milliseconds.
    pub const MAX_MILLISECONDS: u16 = 1_200;
    /// Factory menu default in milliseconds.
    pub const FACTORY_DEFAULT_MILLISECONDS: u16 = 200;

    /// Returns the documented TH-D75 factory menu value.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self(20)
    }

    /// Construct a delay expressed in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidKissTiming`] unless the duration is
    /// in `0..=1200` and divisible by 10.
    pub const fn from_milliseconds(milliseconds: u16) -> Result<Self, ValidationError> {
        match encode_timing(milliseconds, Self::MAX_MILLISECONDS) {
            Some(wire_value) => Ok(Self(wire_value)),
            None => Err(ValidationError::InvalidKissTiming {
                parameter: "KISS TX delay",
                milliseconds,
                maximum_milliseconds: Self::MAX_MILLISECONDS,
            }),
        }
    }

    /// Return the configured duration in milliseconds.
    #[must_use]
    pub const fn as_milliseconds(self) -> u16 {
        decode_timing(self.0)
    }

    /// Return the one-byte KISS representation.
    #[must_use]
    pub const fn to_wire_byte(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for KissTxDelay {
    type Error = ValidationError;

    fn try_from(wire_value: u8) -> Result<Self, Self::Error> {
        Self::from_milliseconds(u16::from(wire_value) * MILLISECONDS_PER_WIRE_UNIT)
    }
}

impl From<KissTxDelay> for u8 {
    fn from(delay: KissTxDelay) -> Self {
        delay.to_wire_byte()
    }
}

impl fmt::Display for KissTxDelay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ms", self.as_milliseconds())
    }
}

/// KISS CSMA persistence value.
///
/// The transmission probability for a clear slot is `(value + 1) / 256`.
/// Every byte value is defined by KISS, so construction is infallible while
/// the newtype prevents confusion with unrelated byte parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KissPersistence(u8);

impl KissPersistence {
    /// TH-D75 factory-default persistence byte.
    pub const FACTORY_DEFAULT_WIRE_VALUE: u8 = 128;

    /// Returns the documented TH-D75 factory value.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self(Self::FACTORY_DEFAULT_WIRE_VALUE)
    }

    /// Construct a persistence value from its KISS byte.
    #[must_use]
    pub const fn new(wire_value: u8) -> Self {
        Self(wire_value)
    }

    /// Return the numerator of the transmission probability over 256.
    #[must_use]
    pub const fn probability_numerator(self) -> u16 {
        u16::from_le_bytes([self.0, 0]) + 1
    }

    /// Return the one-byte KISS representation.
    #[must_use]
    pub const fn to_wire_byte(self) -> u8 {
        self.0
    }
}

impl From<u8> for KissPersistence {
    fn from(wire_value: u8) -> Self {
        Self::new(wire_value)
    }
}

impl From<KissPersistence> for u8 {
    fn from(persistence: KissPersistence) -> Self {
        persistence.to_wire_byte()
    }
}

impl fmt::Display for KissPersistence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/256", self.probability_numerator())
    }
}

/// KISS CSMA slot duration for the TH-D75.
///
/// The radio accepts 0 through 2500 milliseconds in 10 millisecond steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KissSlotTime(u8);

impl KissSlotTime {
    /// Maximum accepted slot duration in milliseconds.
    pub const MAX_MILLISECONDS: u16 = 2_500;
    /// TH-D75 factory-default slot duration in milliseconds.
    pub const FACTORY_DEFAULT_MILLISECONDS: u16 = 100;

    /// Returns the documented TH-D75 factory value.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self(10)
    }

    /// Construct a slot duration expressed in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidKissTiming`] unless the duration is
    /// in `0..=2500` and divisible by 10.
    pub const fn from_milliseconds(milliseconds: u16) -> Result<Self, ValidationError> {
        match encode_timing(milliseconds, Self::MAX_MILLISECONDS) {
            Some(wire_value) => Ok(Self(wire_value)),
            None => Err(ValidationError::InvalidKissTiming {
                parameter: "KISS slot time",
                milliseconds,
                maximum_milliseconds: Self::MAX_MILLISECONDS,
            }),
        }
    }

    /// Return the configured duration in milliseconds.
    #[must_use]
    pub const fn as_milliseconds(self) -> u16 {
        decode_timing(self.0)
    }

    /// Return the one-byte KISS representation.
    #[must_use]
    pub const fn to_wire_byte(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for KissSlotTime {
    type Error = ValidationError;

    fn try_from(wire_value: u8) -> Result<Self, Self::Error> {
        Self::from_milliseconds(u16::from(wire_value) * MILLISECONDS_PER_WIRE_UNIT)
    }
}

impl From<KissSlotTime> for u8 {
    fn from(slot_time: KissSlotTime) -> Self {
        slot_time.to_wire_byte()
    }
}

impl fmt::Display for KissSlotTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ms", self.as_milliseconds())
    }
}

/// KISS transmitter tail duration.
///
/// KISS allocates the full byte range, representing 0 through 2550
/// milliseconds in 10 millisecond steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KissTxTail(u8);

impl KissTxTail {
    /// Maximum representable tail duration in milliseconds.
    pub const MAX_MILLISECONDS: u16 = 2_550;
    /// TH-D75 factory-default tail duration in milliseconds.
    pub const FACTORY_DEFAULT_MILLISECONDS: u16 = 30;

    /// Returns the documented TH-D75 factory value.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self(3)
    }

    /// Construct a tail duration expressed in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidKissTiming`] unless the duration is
    /// in `0..=2550` and divisible by 10.
    pub const fn from_milliseconds(milliseconds: u16) -> Result<Self, ValidationError> {
        match encode_timing(milliseconds, Self::MAX_MILLISECONDS) {
            Some(wire_value) => Ok(Self(wire_value)),
            None => Err(ValidationError::InvalidKissTiming {
                parameter: "KISS TX tail",
                milliseconds,
                maximum_milliseconds: Self::MAX_MILLISECONDS,
            }),
        }
    }

    /// Return the configured duration in milliseconds.
    #[must_use]
    pub const fn as_milliseconds(self) -> u16 {
        decode_timing(self.0)
    }

    /// Return the one-byte KISS representation.
    #[must_use]
    pub const fn to_wire_byte(self) -> u8 {
        self.0
    }
}

impl From<u8> for KissTxTail {
    fn from(wire_value: u8) -> Self {
        Self(wire_value)
    }
}

impl From<KissTxTail> for u8 {
    fn from(tx_tail: KissTxTail) -> Self {
        tx_tail.to_wire_byte()
    }
}

impl fmt::Display for KissTxTail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ms", self.as_milliseconds())
    }
}

/// KISS duplex behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KissDuplex {
    /// Wait for a clear channel before transmitting.
    Half,
    /// Permit transmission without half-duplex carrier deferral.
    Full,
}

impl KissDuplex {
    /// Returns the documented TH-D75 KISS-mode factory value.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self::Half
    }

    /// Return the one-byte KISS representation.
    #[must_use]
    pub const fn to_wire_byte(self) -> u8 {
        match self {
            Self::Half => 0,
            Self::Full => 1,
        }
    }
}

impl From<KissDuplex> for u8 {
    fn from(duplex: KissDuplex) -> Self {
        duplex.to_wire_byte()
    }
}

impl fmt::Display for KissDuplex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Half => formatter.write_str("half duplex"),
            Self::Full => formatter.write_str("full duplex"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn tx_delay_uses_real_time_and_rejects_invalid_steps() -> TestResult {
        let delay = KissTxDelay::from_milliseconds(500)?;
        assert_eq!(delay.as_milliseconds(), 500);
        assert_eq!(delay.to_wire_byte(), 50);
        assert!(KissTxDelay::from_milliseconds(1_210).is_err());
        assert!(KissTxDelay::from_milliseconds(505).is_err());
        assert!(KissTxDelay::try_from(121).is_err());
        Ok(())
    }

    #[test]
    fn slot_time_accepts_its_complete_radio_domain() -> TestResult {
        assert_eq!(KissSlotTime::from_milliseconds(0)?.to_wire_byte(), 0);
        assert_eq!(
            KissSlotTime::from_milliseconds(KissSlotTime::MAX_MILLISECONDS)?.to_wire_byte(),
            250
        );
        assert!(KissSlotTime::from_milliseconds(2_510).is_err());
        assert!(KissSlotTime::try_from(251).is_err());
        Ok(())
    }

    #[test]
    fn tx_tail_and_persistence_cover_all_wire_bytes() -> TestResult {
        assert_eq!(
            KissTxTail::from_milliseconds(KissTxTail::MAX_MILLISECONDS)?.to_wire_byte(),
            u8::MAX
        );
        assert!(KissTxTail::from_milliseconds(31).is_err());
        assert_eq!(KissTxTail::from(u8::MAX).as_milliseconds(), 2_550);
        assert_eq!(KissPersistence::from(u8::MAX).probability_numerator(), 256);
        assert_eq!(KissPersistence::from(0).probability_numerator(), 1);
        Ok(())
    }

    #[test]
    fn explicit_factory_defaults_match_documented_thd75_values() {
        assert_eq!(KissTxDelay::factory_default().as_milliseconds(), 200);
        assert_eq!(KissPersistence::factory_default().to_wire_byte(), 128);
        assert_eq!(KissSlotTime::factory_default().as_milliseconds(), 100);
        assert_eq!(KissTxTail::factory_default().as_milliseconds(), 30);
        assert_eq!(KissDuplex::factory_default(), KissDuplex::Half);
    }
}
