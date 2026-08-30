//! Radio identity: model, market type, firmware.

use std::fmt;
use std::str::FromStr;

use crate::error::ValidationError;

/// Radio model accepted by this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RadioModel {
    /// Kenwood TM-D750 (every market variant answers `ID` with the same string).
    TmD750,
}

impl RadioModel {
    /// Exact CAT `ID` payload for a TM-D750.
    pub const TM_D750_ID: &str = "TM-D750";

    /// The exact CAT model identity.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TmD750 => Self::TM_D750_ID,
        }
    }
}

impl TryFrom<&str> for RadioModel {
    type Error = ValidationError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value == Self::TM_D750_ID {
            Ok(Self::TmD750)
        } else {
            Err(ValidationError::UnsupportedRadioModel {
                model: value.to_owned(),
            })
        }
    }
}

impl fmt::Display for RadioModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The single printable byte the `TY` command reports.
///
/// The official program recognizes three values ([`KNOWN_TYPE_BYTES`]);
/// what they select is a day-one hardware finding recorded in the crate
/// notes, so this type carries the byte without naming its meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MarketType(u8);

/// Type bytes the official program recognizes.
pub const KNOWN_TYPE_BYTES: [u8; 3] = [b'J', b'0', b'1'];

impl MarketType {
    /// Validate a `TY` payload byte (printable ASCII).
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidMarketTypeByte`] for a non-printable byte.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value.is_ascii_graphic() {
            Ok(Self(value))
        } else {
            Err(ValidationError::InvalidMarketTypeByte { value })
        }
    }

    /// The raw byte.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        self.0
    }

    /// Whether the official program recognizes this byte.
    #[must_use]
    pub fn is_known(self) -> bool {
        KNOWN_TYPE_BYTES.contains(&self.0)
    }
}

impl fmt::Display for MarketType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", char::from(self.0))
    }
}

/// Exact, bounded token returned by the CAT `FV` command.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FirmwareIdentity(String);

impl FirmwareIdentity {
    /// Maximum byte length of the field.
    pub const MAX_LEN: usize = 8;

    /// Validate and copy an exact `FV` payload.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::FirmwareIdentityLength`] for an empty or
    /// overlong token and [`ValidationError::InvalidFirmwareIdentityByte`]
    /// for a byte that is not printable ASCII.
    pub fn new(value: &str) -> Result<Self, ValidationError> {
        if !(1..=Self::MAX_LEN).contains(&value.len()) {
            return Err(ValidationError::FirmwareIdentityLength {
                len: value.len(),
                max: Self::MAX_LEN,
            });
        }
        if let Some((offset, byte)) = value
            .bytes()
            .enumerate()
            .find(|(_, byte)| !byte.is_ascii_graphic())
        {
            return Err(ValidationError::InvalidFirmwareIdentityByte {
                offset,
                value: byte,
            });
        }
        Ok(Self(value.to_owned()))
    }

    /// The exact payload.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for FirmwareIdentity {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl fmt::Display for FirmwareIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn model_accepts_only_the_exact_string() {
        assert_eq!(RadioModel::try_from("TM-D750"), Ok(RadioModel::TmD750));
        let rejected = RadioModel::try_from("TH-D75");
        assert!(
            matches!(rejected, Err(ValidationError::UnsupportedRadioModel { .. })),
            "{rejected:?}"
        );
    }

    #[test]
    fn market_type_is_a_printable_byte() -> TestResult {
        let known = MarketType::new(b'J')?;
        assert!(known.is_known());
        assert_eq!(known.to_string(), "J");
        assert!(!MarketType::new(b'X')?.is_known());
        let rejected = MarketType::new(0x0D);
        assert!(
            matches!(
                rejected,
                Err(ValidationError::InvalidMarketTypeByte { value: 0x0D })
            ),
            "{rejected:?}"
        );
        Ok(())
    }

    #[test]
    fn firmware_identity_bounds_and_bytes() -> TestResult {
        assert_eq!(FirmwareIdentity::new("1.00")?.as_str(), "1.00");
        let long = FirmwareIdentity::new("123456789");
        assert!(
            matches!(
                long,
                Err(ValidationError::FirmwareIdentityLength { len: 9, max: 8 })
            ),
            "{long:?}"
        );
        let space = FirmwareIdentity::new("1 0");
        assert!(
            matches!(
                space,
                Err(ValidationError::InvalidFirmwareIdentityByte { offset: 1, .. })
            ),
            "{space:?}"
        );
        Ok(())
    }
}
