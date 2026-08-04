//! `EchoLink` memory types (Menu No. 164).
//!
//! `EchoLink` is a `VoIP` system that links amateur radio stations over the
//! internet. The TH-D75 supports 10 `EchoLink` memory slots for storing
//! frequently used node numbers and their associated station names for
//! quick access via DTMF dialing.
//!
//! Per User Manual Chapter 11:
//!
//! - `EchoLink` memory channels are separate from normal DTMF memory.
//! - They do NOT store operating frequencies, tones, or power information.
//! - Each slot stores a callsign/name (up to 8 encoded bytes) and a node
//!   number or DTMF code (up to 8 digits).
//! - The radio supports `EchoLink` "Connect by Call" (prefix `C`) and
//!   "Query by Call" (prefix `07`) functions with automatic callsign-to-DTMF
//!   conversion.
//! - When only a name is stored (no code), the "Connect Call" function
//!   automatically converts the callsign to DTMF with `C` prefix and `#` suffix.
//!
//! These types model `EchoLink` settings from the TH-D75's menu system.

use crate::error::ValidationError;

// ---------------------------------------------------------------------------
// EchoLink memory slot
// ---------------------------------------------------------------------------

/// An `EchoLink` memory slot.
///
/// The TH-D75 provides 10 `EchoLink` memory slots (0-9), each storing
/// a station name and node number. Node numbers are dialed via DTMF
/// to connect to the remote `EchoLink` station through a repeater's
/// `EchoLink` interface.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EchoLinkMemory {
    /// Slot index (0-9).
    pub slot: EchoLinkSlot,
    /// Station name or callsign (up to 8 UTF-8 encoded bytes).
    pub name: EchoLinkName,
    /// Stored node number or DTMF control code (up to 8 digits).
    pub code: EchoLinkCode,
}

/// `EchoLink` memory slot index (0-9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EchoLinkSlot(u8);

impl EchoLinkSlot {
    /// Maximum slot index.
    pub const MAX: u8 = 9;

    /// Total number of `EchoLink` memory slots.
    pub const COUNT: usize = 10;

    /// Creates a new `EchoLink` memory slot index.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::EchoLinkSlotOutOfRange`] if the index
    /// exceeds 9.
    pub const fn new(index: u8) -> Result<Self, ValidationError> {
        if index <= Self::MAX {
            Ok(Self(index))
        } else {
            Err(ValidationError::EchoLinkSlotOutOfRange { index })
        }
    }

    /// Returns the slot index.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

/// `EchoLink` station name (up to 8 UTF-8 encoded bytes).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct EchoLinkName(String);

impl EchoLinkName {
    /// Maximum length of an `EchoLink` station name.
    pub const MAX_LEN: usize = 8;

    /// Creates a new `EchoLink` station name.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::EchoLinkNameTooLong`] if `text` exceeds
    /// eight UTF-8 encoded bytes.
    pub fn new(text: &str) -> Result<Self, ValidationError> {
        if text.len() <= Self::MAX_LEN {
            Ok(Self(text.to_owned()))
        } else {
            Err(ValidationError::EchoLinkNameTooLong { len: text.len() })
        }
    }

    /// Returns the name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `EchoLink` node number or DTMF control code (up to 8 digits).
///
/// `EchoLink` node numbers are numeric identifiers assigned to each
/// registered station. They are transmitted via DTMF tones through
/// a repeater to initiate an `EchoLink` connection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct EchoLinkCode(String);

impl EchoLinkCode {
    /// Maximum length of a stored `EchoLink` DTMF code.
    pub const MAX_LEN: usize = 8;

    /// Creates a new `EchoLink` DTMF code.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::EchoLinkCodeTooLong`] if the string exceeds
    /// eight encoded bytes, or [`ValidationError::InvalidEchoLinkCodeDigit`]
    /// at the first character outside `0`-`9`, `A`-`D`, `*`, and `#`.
    pub fn new(code: &str) -> Result<Self, ValidationError> {
        if code.len() > Self::MAX_LEN {
            return Err(ValidationError::EchoLinkCodeTooLong { len: code.len() });
        }
        if let Some((offset, value)) = code
            .char_indices()
            .find(|(_, c)| !super::dtmf::is_valid_dtmf(*c))
        {
            return Err(ValidationError::InvalidEchoLinkCodeDigit { offset, value });
        }
        Ok(Self(code.to_owned()))
    }

    /// Returns the code as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns `true` if the code is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echolink_slot_valid_range() {
        for i in 0u8..=9 {
            assert!(EchoLinkSlot::new(i).is_ok());
        }
    }

    #[test]
    fn echolink_slot_invalid() {
        assert!(matches!(
            EchoLinkSlot::new(10),
            Err(ValidationError::EchoLinkSlotOutOfRange { index: 10 })
        ));
    }

    #[test]
    fn echolink_slot_index() -> Result<(), Box<dyn std::error::Error>> {
        let slot = EchoLinkSlot::new(5)?;
        assert_eq!(slot.as_raw(), 5);
        Ok(())
    }

    #[test]
    fn echolink_name_valid() -> Result<(), Box<dyn std::error::Error>> {
        let name = EchoLinkName::new("W1AW")?;
        assert_eq!(name.as_str(), "W1AW");
        Ok(())
    }

    #[test]
    fn echolink_name_max_length() -> Result<(), Box<dyn std::error::Error>> {
        let name = EchoLinkName::new("12345678")?;
        assert_eq!(name.as_str().len(), 8);
        Ok(())
    }

    #[test]
    fn echolink_name_too_long() {
        assert!(matches!(
            EchoLinkName::new("123456789"),
            Err(ValidationError::EchoLinkNameTooLong { len: 9 })
        ));
    }

    #[test]
    fn echolink_code_valid() -> Result<(), Box<dyn std::error::Error>> {
        let code = EchoLinkCode::new("12A*34#D")?;
        assert_eq!(code.as_str(), "12A*34#D");
        assert!(!code.is_empty());
        Ok(())
    }

    #[test]
    fn echolink_code_short() -> Result<(), Box<dyn std::error::Error>> {
        let code = EchoLinkCode::new("1")?;
        assert_eq!(code.as_str(), "1");
        Ok(())
    }

    #[test]
    fn echolink_code_empty() -> Result<(), Box<dyn std::error::Error>> {
        let code = EchoLinkCode::new("")?;
        assert!(code.is_empty());
        Ok(())
    }

    #[test]
    fn echolink_code_too_long() {
        assert!(matches!(
            EchoLinkCode::new("123456789"),
            Err(ValidationError::EchoLinkCodeTooLong { len: 9 })
        ));
    }

    #[test]
    fn echolink_code_rejects_non_dtmf_letter() {
        assert!(matches!(
            EchoLinkCode::new("12E456"),
            Err(ValidationError::InvalidEchoLinkCodeDigit {
                offset: 2,
                value: 'E'
            })
        ));
    }

    #[test]
    fn echolink_code_rejects_space() {
        assert!(matches!(
            EchoLinkCode::new("12 456"),
            Err(ValidationError::InvalidEchoLinkCodeDigit {
                offset: 2,
                value: ' '
            })
        ));
    }
}
