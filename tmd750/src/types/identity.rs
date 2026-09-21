//! Radio identity: model, opaque radio type, and firmware.

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

/// Exact opaque payload returned by the CAT `TY` query.
///
/// A stock North American TM-D750 running firmware 1.02 returns `K,2,1`.
/// The meanings of its components are unknown. Other hardware variants may use
/// a different graphic-ASCII shape, which is retained unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RadioType(String);

impl RadioType {
    /// Retain a non-empty graphic-ASCII payload (`0x21..=0x7E`) exactly.
    ///
    /// Spaces, other whitespace, controls, and non-ASCII bytes are rejected;
    /// no trimming, normalization, or component interpretation occurs.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidRadioTypePayload`] for an empty value
    /// or one containing a byte outside that range.
    pub fn new(payload: &str) -> Result<Self, ValidationError> {
        if payload.is_empty() || !payload.bytes().all(|byte| byte.is_ascii_graphic()) {
            Err(ValidationError::InvalidRadioTypePayload {
                payload: payload.to_owned(),
            })
        } else {
            Ok(Self(payload.to_owned()))
        }
    }

    /// The exact `TY` payload.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RadioType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
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
    /// Requires one through [`Self::MAX_LEN`] graphic-ASCII bytes
    /// (`0x21..=0x7E`). Spaces and all other whitespace are rejected, not trimmed.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::FirmwareIdentityLength`] for an empty or
    /// overlong token and [`ValidationError::InvalidFirmwareIdentityByte`]
    /// for a byte outside graphic ASCII.
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
    fn radio_type_preserves_the_exact_printable_payload() -> TestResult {
        let observed = RadioType::new("K,2,1")?;
        assert_eq!(observed.as_str(), "K,2,1");
        assert_eq!(observed.to_string(), "K,2,1");
        assert_eq!(RadioType::new("J")?.as_str(), "J");
        let rejected = RadioType::new("");
        assert!(
            matches!(
                rejected,
                Err(ValidationError::InvalidRadioTypePayload { .. })
            ),
            "{rejected:?}"
        );
        assert!(RadioType::new("K,\r,1").is_err());
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
