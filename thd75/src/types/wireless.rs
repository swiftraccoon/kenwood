//! Wireless remote control types (TH-D75A only).
//!
//! The TH-D75A supports wireless remote control of a Kenwood multi-band
//! mobile transceiver via DTMF signaling. A "control" radio sends
//! DTMF commands over air to a "target" radio, which decodes them and
//! executes the corresponding function. Access is protected by a
//! 3-digit secret access code (Menu No. 946, range 000-999).
//!
//! Per User Manual Chapter 25:
//!
//! - FCC rules permit sending control codes only on the 440 MHz band.
//! - The target mobile transceiver must have both the secret number and
//!   Remote Control functions.
//! - The DTMF format is `AXXX#YA#` where `XXX` is the 3-digit secret
//!   code and `Y` is a single-digit control command.
//!
//! # Remote control commands (per User Manual Chapter 25)
//!
//! | RM# | Name | Operation |
//! |-----|------|-----------|
//! | RM0 | LOW | Toggle TX power |
//! | RM1 | On | DCS ON / Reverse ON / Tone Alert ON |
//! | RM2 | TONE On | Tone ON |
//! | RM3 | CTCSS On | CTCSS ON |
//! | RM4 | Off | DCS OFF / Reverse OFF / Tone Alert OFF |
//! | RM5 | TONE Off | Tone OFF |
//! | RM6 | CTCSS Off | CTCSS OFF |
//! | RM7 | CALL | Call mode ON |
//! | RM8 | VFO | VFO mode ON |
//! | RM9 | MR | Memory mode ON |
//! | RMA | Freq. Enter | Frequency or channel direct entry |
//! | RMB | Tone Select | DCS code / Tone freq / CTCSS freq setup |
//! | RMC | REPEATER On | Repeater ON |
//! | RMD | REPEATER Off | Repeater OFF |
//! | RM\* | DOWN | Step frequency/channel down |
//! | RM# | UP | Step frequency/channel up |
//!
//! [`RemoteControlCode`] models the three-digit value stored by Menu No. 946.

use crate::error::ValidationError;

// ---------------------------------------------------------------------------
// Wireless control code
// ---------------------------------------------------------------------------

/// Wireless remote-control secret code (Menu No. 946).
///
/// The user manual and MCP memory schema agree that this is exactly three
/// decimal digits (`000` through `999`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RemoteControlCode(String);

impl RemoteControlCode {
    /// Required code length in encoded bytes.
    pub const LEN: usize = 3;

    /// Creates a wireless remote-control secret code.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::RemoteControlCodeLength`] unless `code` is
    /// exactly three encoded bytes, or
    /// [`ValidationError::InvalidRemoteControlCodeDigit`] at the first
    /// non-decimal character.
    pub fn new(code: &str) -> Result<Self, ValidationError> {
        if code.len() != Self::LEN {
            return Err(ValidationError::RemoteControlCodeLength { len: code.len() });
        }
        if let Some((offset, value)) = code
            .char_indices()
            .find(|(_, character)| !character.is_ascii_digit())
        {
            return Err(ValidationError::InvalidRemoteControlCodeDigit { offset, value });
        }
        Ok(Self(code.to_owned()))
    }

    /// Returns the three-digit code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_control_code_valid() -> Result<(), Box<dyn std::error::Error>> {
        let code = RemoteControlCode::new("123")?;
        assert_eq!(code.as_str(), "123");
        Ok(())
    }

    #[test]
    fn remote_control_code_accepts_leading_zeroes() -> Result<(), Box<dyn std::error::Error>> {
        let code = RemoteControlCode::new("007")?;
        assert_eq!(code.as_str(), "007");
        Ok(())
    }

    #[test]
    fn remote_control_code_too_short() {
        assert!(matches!(
            RemoteControlCode::new("12"),
            Err(ValidationError::RemoteControlCodeLength { len: 2 })
        ));
    }

    #[test]
    fn remote_control_code_too_long() {
        assert!(matches!(
            RemoteControlCode::new("1234"),
            Err(ValidationError::RemoteControlCodeLength { len: 4 })
        ));
    }

    #[test]
    fn remote_control_code_rejects_non_decimal_characters() {
        assert!(matches!(
            RemoteControlCode::new("12A"),
            Err(ValidationError::InvalidRemoteControlCodeDigit {
                offset: 2,
                value: 'A'
            })
        ));
        assert!(matches!(
            RemoteControlCode::new("1#3"),
            Err(ValidationError::InvalidRemoteControlCodeDigit {
                offset: 1,
                value: '#'
            })
        ));
    }
}
