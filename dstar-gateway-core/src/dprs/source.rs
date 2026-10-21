//! DPRS source identifiers in the TNC2 text envelope.

use std::fmt;
use std::str::FromStr;

use crate::types::{Callsign, WireTextError};

/// Uppercase DPRS source identifier, including an optional module or numeric SSID.
///
/// An unqualified identifier has 1..=8 ASCII letters/digits, accommodating the
/// D-STAR field. A suffixed identifier has a 1..=6-byte base and either one
/// module letter or a decimal SSID from 0 through 15. The result occupies at
/// most nine bytes. `ircDDBGateway/Common/APRSCollector.cpp::dstarCallsignToAPRS`
/// converts a separated D-STAR module to this hyphenated form.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DprsSource(String);

impl DprsSource {
    /// Validate a complete source identifier and uppercase its ASCII letters.
    ///
    /// # Errors
    ///
    /// Returns [`DprsSourceError`] for invalid width, base bytes, suffix grammar,
    /// or a numeric SSID above 15. Padding is not part of this constructor.
    pub fn new(value: &str) -> Result<Self, DprsSourceError> {
        let (base, suffix) = value
            .split_once('-')
            .map_or((value, None), |(base, suffix)| (base, Some(suffix)));
        let maximum = if suffix.is_some() { 6 } else { 8 };
        if base.is_empty() || base.len() > maximum {
            return Err(DprsSourceError::BaseLength {
                got: base.len(),
                maximum,
            });
        }
        if let Some((index, byte)) = base
            .bytes()
            .enumerate()
            .find(|(_, byte)| !byte.is_ascii_alphanumeric())
        {
            return Err(DprsSourceError::BaseByte { index, byte });
        }
        if let Some(suffix) = suffix {
            let module = suffix.len() == 1 && suffix.bytes().all(|byte| byte.is_ascii_alphabetic());
            let numeric = (1..=2).contains(&suffix.len())
                && suffix.bytes().all(|byte| byte.is_ascii_digit())
                && suffix.parse::<u8>().is_ok_and(|value| value <= 15);
            if !module && !numeric {
                return Err(DprsSourceError::Suffix);
            }
        }
        Ok(Self(value.to_ascii_uppercase()))
    }

    /// Borrow the complete canonical source identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for DprsSource {
    type Err = DprsSourceError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for DprsSource {
    type Error = DprsSourceError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<Callsign> for DprsSource {
    type Error = DprsSourceError;

    fn try_from(value: Callsign) -> Result<Self, Self::Error> {
        let text = value.text()?;
        match text.split_once(' ') {
            Some((base, module)) => {
                Self::new(&format!("{base}-{}", module.trim_start_matches(' ')))
            }
            None => Self::new(text),
        }
    }
}

impl AsRef<str> for DprsSource {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl From<DprsSource> for String {
    fn from(value: DprsSource) -> Self {
        value.0
    }
}

impl fmt::Display for DprsSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A DPRS source that cannot be encoded in the supported TNC2 identifier grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DprsSourceError {
    /// Base identifier is empty or exceeds its permitted byte width.
    #[error("DPRS source base has {got} bytes; expected 1..={maximum}")]
    BaseLength {
        /// Observed base length.
        got: usize,
        /// Maximum base length for the selected suffix form.
        maximum: usize,
    },
    /// Base identifier contains a non-alphanumeric byte.
    #[error("DPRS source base byte {index} is invalid: 0x{byte:02X}")]
    BaseByte {
        /// Zero-based byte index.
        index: usize,
        /// Rejected original byte.
        byte: u8,
    },
    /// Suffix is not one module letter or a decimal SSID from 0 through 15.
    #[error("DPRS source suffix must be one module letter or an SSID from 0 through 15")]
    Suffix,
    /// A raw receive callsign contains a non-printable byte.
    #[error(transparent)]
    Wire(#[from] WireTextError),
}
